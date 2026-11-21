import {
  CHANGE_RETENTION_MS,
  JOB_LEASE_MS,
  JOB_RETRY_MS,
  MAINTENANCE_PAGE_SIZE,
  type ApiErrorCode,
} from "./constants";
import {
  claimJob,
  dueJobIds,
  planFromJson,
  r2ChunkKey,
  uploadSession,
} from "./db";
import { ApiProblem, logMaintenance, problem } from "./errors";
import { isJsonRecord, isJsonString } from "./json";
import type { Env, MaintenanceJobRow, UploadSessionRow } from "./types";

interface StoredChunkRow {
  byte_size: number;
  chunk_index: number;
  digest: string;
  r2_key: string;
}

function assertJobPayload(record: MaintenanceJobRow): void {
  let parsed: unknown;
  try {
    parsed = JSON.parse(record.payload_json);
  } catch {
    throw problem("integrity_failed");
  }
  if (!isJsonRecord(parsed)) throw problem("integrity_failed");
  for (const value of Object.values(parsed)) {
    if (!isJsonString(value)) throw problem("integrity_failed");
  }
}

async function markDone(
  env: Env,
  job: MaintenanceJobRow,
  now: number,
): Promise<void> {
  await env.DB.prepare(
    "UPDATE maintenance_jobs SET state = 'done', lease_until = NULL, lease_token = NULL, updated_at = ?, last_error_code = NULL WHERE job_id = ? AND state = 'running' AND lease_token = ?",
  )
    .bind(now, job.job_id, job.lease_token)
    .run();
}

async function retryLater(
  env: Env,
  job: MaintenanceJobRow,
  now: number,
  errorCode: ApiErrorCode,
  progress = job.progress,
): Promise<void> {
  await env.DB.prepare(
    "UPDATE maintenance_jobs SET state = 'queued', lease_until = NULL, lease_token = NULL, progress = ?, next_attempt_at = ?, last_error_code = ?, updated_at = ? WHERE job_id = ? AND state = 'running' AND lease_token = ?",
  )
    .bind(
      progress,
      now + JOB_RETRY_MS,
      errorCode,
      now,
      job.job_id,
      job.lease_token,
    )
    .run();
}

async function updateProgress(
  env: Env,
  job: MaintenanceJobRow,
  progress: number,
  now: number,
): Promise<void> {
  await env.DB.prepare(
    "UPDATE maintenance_jobs SET state = 'queued', lease_until = NULL, lease_token = NULL, progress = ?, next_attempt_at = ?, updated_at = ? WHERE job_id = ? AND state = 'running' AND lease_token = ?",
  )
    .bind(progress, now, now, job.job_id, job.lease_token)
    .run();
}

async function releaseUploadUsage(
  env: Env,
  job: MaintenanceJobRow,
  session: UploadSessionRow,
  now: number,
): Promise<void> {
  await env.DB.batch([
    env.DB.prepare(
      "UPDATE upload_sessions SET cleanup_accounted = 1, state = 'purged', updated_at = ? WHERE upload_id = ? AND cleanup_accounted = 0 AND EXISTS (SELECT 1 FROM maintenance_jobs WHERE job_id = ? AND state = 'running' AND lease_token = ?)",
    ).bind(now, session.upload_id, job.job_id, job.lease_token),
    env.DB.prepare(
      "UPDATE vaults SET used_bytes = CASE WHEN used_bytes >= ? THEN used_bytes - ? ELSE 0 END WHERE vault_id = ? AND changes() = 1 AND EXISTS (SELECT 1 FROM maintenance_jobs WHERE job_id = ? AND state = 'running' AND lease_token = ?)",
    ).bind(
      session.total_bytes,
      session.total_bytes,
      session.vault_id,
      job.job_id,
      job.lease_token,
    ),
  ]);
}

async function cleanUpload(
  env: Env,
  job: MaintenanceJobRow,
  now: number,
): Promise<boolean> {
  const session = await uploadSession(env, job.vault_id, job.target_id);
  if (session === null || session.cleanup_accounted === 1) return true;
  const plan = planFromJson(session.chunk_plan);
  const end = Math.min(job.progress + MAINTENANCE_PAGE_SIZE, plan.length);
  const chunks = plan.slice(job.progress, end);
  for (const chunk of chunks) {
    if (chunk === undefined) throw problem("integrity_failed");
  }
  await Promise.all(
    chunks.map((chunk) =>
      env.CIPHERTEXT.delete(
        r2ChunkKey(job.vault_id, session.upload_id, chunk.index),
      ),
    ),
  );
  if (end < plan.length) {
    await updateProgress(env, job, end, now);
    return false;
  }
  await releaseUploadUsage(env, job, session, now);
  return true;
}

async function cleanShare(
  env: Env,
  job: MaintenanceJobRow,
  now: number,
): Promise<void> {
  const share = await env.DB.prepare(
    "SELECT upload_id FROM shares WHERE vault_id = ? AND share_id = ?",
  )
    .bind(job.vault_id, job.target_id)
    .first<{ upload_id: string }>();
  if (share === null) {
    await markDone(env, job, now);
    return;
  }
  const shareJob: MaintenanceJobRow = { ...job, target_id: share.upload_id };
  if (!(await cleanUpload(env, shareJob, now))) return;
  const session = await uploadSession(env, job.vault_id, share.upload_id);
  if (session?.cleanup_accounted === 1) {
    await env.DB.prepare(
      "UPDATE shares SET state = 'purged' WHERE vault_id = ? AND share_id = ? AND EXISTS (SELECT 1 FROM maintenance_jobs WHERE job_id = ? AND state = 'running' AND lease_token = ?)",
    )
      .bind(job.vault_id, job.target_id, job.job_id, job.lease_token)
      .run();
  }
  await markDone(env, job, now);
}

async function cleanObject(
  env: Env,
  job: MaintenanceJobRow,
  now: number,
): Promise<void> {
  const candidates = await env.DB.prepare(
    "SELECT rc.chunk_index, rc.r2_key, rc.byte_size, rc.digest FROM revision_chunks rc WHERE rc.vault_id = ? AND rc.object_id = ? AND rc.purged_at IS NULL AND rc.revision_id <> COALESCE((SELECT head_revision_id FROM object_heads WHERE vault_id = ? AND object_id = ? AND tombstone = 0), '') ORDER BY rc.revision_id, rc.chunk_index LIMIT ?",
  )
    .bind(
      job.vault_id,
      job.target_id,
      job.vault_id,
      job.target_id,
      MAINTENANCE_PAGE_SIZE,
    )
    .all<StoredChunkRow>();
  await Promise.all(
    candidates.results.map((chunk) => env.CIPHERTEXT.delete(chunk.r2_key)),
  );
  if (candidates.results.length > 0) {
    const statements: D1PreparedStatement[] = [];
    for (const chunk of candidates.results) {
      statements.push(
        env.DB.prepare(
          "UPDATE revision_chunks SET purged_at = ? WHERE r2_key = ? AND purged_at IS NULL AND EXISTS (SELECT 1 FROM maintenance_jobs WHERE job_id = ? AND state = 'running' AND lease_token = ?)",
        ).bind(now, chunk.r2_key, job.job_id, job.lease_token),
        env.DB.prepare(
          "UPDATE vaults SET used_bytes = CASE WHEN used_bytes >= ? THEN used_bytes - ? ELSE 0 END WHERE vault_id = ? AND changes() = 1 AND EXISTS (SELECT 1 FROM maintenance_jobs WHERE job_id = ? AND state = 'running' AND lease_token = ?)",
        ).bind(
          chunk.byte_size,
          chunk.byte_size,
          job.vault_id,
          job.job_id,
          job.lease_token,
        ),
      );
    }
    statements.push(
      env.DB.prepare(
        "UPDATE maintenance_jobs SET state = 'queued', lease_until = NULL, lease_token = NULL, progress = progress + ?, next_attempt_at = ?, updated_at = ? WHERE job_id = ? AND state = 'running' AND lease_token = ?",
      ).bind(candidates.results.length, now, now, job.job_id, job.lease_token),
    );
    await env.DB.batch(statements);
    return;
  }
  await markDone(env, job, now);
}

async function reconcileUpload(
  env: Env,
  job: MaintenanceJobRow,
  now: number,
): Promise<void> {
  const session = await uploadSession(env, job.vault_id, job.target_id);
  if (session === null || session.state !== "active") {
    await markDone(env, job, now);
    return;
  }
  const plan = planFromJson(session.chunk_plan);
  if (plan.length === 0) {
    await markDone(env, job, now);
    return;
  }
  const accepted = await env.DB.prepare(
    "SELECT chunk_index FROM upload_chunks WHERE upload_id = ?",
  )
    .bind(session.upload_id)
    .all<{ chunk_index: number }>();
  const acceptedIndexes = new Set(
    accepted.results.map((row) => row.chunk_index),
  );
  let cursor = job.progress % plan.length;
  let inspected = 0;
  while (inspected < MAINTENANCE_PAGE_SIZE && inspected < plan.length) {
    const chunk = plan[cursor];
    if (chunk === undefined) throw problem("integrity_failed");
    if (!acceptedIndexes.has(chunk.index)) {
      const key = r2ChunkKey(job.vault_id, session.upload_id, chunk.index);
      const stored = await env.CIPHERTEXT.head(key);
      if (stored !== null) {
        if (
          stored.size !== chunk.size ||
          stored.customMetadata?.sha256 !== chunk.sha256
        ) {
          throw problem("integrity_failed");
        }
        await env.DB.prepare(
          "INSERT OR IGNORE INTO upload_chunks (upload_id, chunk_index, byte_size, digest, r2_key, accepted_at) SELECT ?, ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'active') AND EXISTS (SELECT 1 FROM maintenance_jobs WHERE job_id = ? AND state = 'running' AND lease_token = ?)",
        )
          .bind(
            session.upload_id,
            chunk.index,
            chunk.size,
            chunk.sha256,
            key,
            now,
            session.upload_id,
            job.job_id,
            job.lease_token,
          )
          .run();
      }
    }
    cursor = (cursor + 1) % plan.length;
    inspected += 1;
  }
  const count = await env.DB.prepare(
    "SELECT COUNT(*) AS accepted_count FROM upload_chunks WHERE upload_id = ?",
  )
    .bind(session.upload_id)
    .first<{ accepted_count: number }>();
  if (count?.accepted_count === plan.length) {
    await markDone(env, job, now);
    return;
  }
  await retryLater(env, job, now, "dependency_unavailable", cursor);
}

async function compactChanges(
  env: Env,
  job: MaintenanceJobRow,
  now: number,
): Promise<void> {
  const cutoff = now - CHANGE_RETENTION_MS;
  const expired = await env.DB.prepare(
    "SELECT sequence FROM changes WHERE vault_id = ? AND created_at < ? ORDER BY sequence LIMIT ?",
  )
    .bind(job.vault_id, cutoff, MAINTENANCE_PAGE_SIZE)
    .all<{ sequence: number }>();
  const hasMore = expired.results.length === MAINTENANCE_PAGE_SIZE;
  const state = hasMore
    ? env.DB.prepare(
        "UPDATE maintenance_jobs SET state = 'queued', lease_until = NULL, lease_token = NULL, progress = progress + ?, next_attempt_at = ?, last_error_code = NULL, updated_at = ? WHERE job_id = ? AND state = 'running' AND lease_token = ?",
      ).bind(expired.results.length, now, now, job.job_id, job.lease_token)
    : env.DB.prepare(
        "UPDATE maintenance_jobs SET state = 'done', lease_until = NULL, lease_token = NULL, last_error_code = NULL, updated_at = ? WHERE job_id = ? AND state = 'running' AND lease_token = ?",
      ).bind(now, job.job_id, job.lease_token);
  await env.DB.batch([
    env.DB.prepare(
      "DELETE FROM changes WHERE rowid IN (SELECT rowid FROM changes WHERE vault_id = ? AND created_at < ? ORDER BY sequence LIMIT ?) AND EXISTS (SELECT 1 FROM maintenance_jobs WHERE job_id = ? AND state = 'running' AND lease_token = ?)",
    ).bind(
      job.vault_id,
      cutoff,
      MAINTENANCE_PAGE_SIZE,
      job.job_id,
      job.lease_token,
    ),
    env.DB.prepare(
      "UPDATE vaults SET min_change_sequence = COALESCE((SELECT MIN(sequence) FROM changes WHERE vault_id = ?), next_change_sequence + 1) WHERE vault_id = ? AND EXISTS (SELECT 1 FROM maintenance_jobs WHERE job_id = ? AND state = 'running' AND lease_token = ?)",
    ).bind(job.vault_id, job.vault_id, job.job_id, job.lease_token),
    state,
  ]);
}

async function processClaimedJob(
  env: Env,
  job: MaintenanceJobRow,
  now: number,
): Promise<void> {
  assertJobPayload(job);
  if (job.job_class === "cleanup_upload") {
    if (await cleanUpload(env, job, now)) await markDone(env, job, now);
    return;
  }
  if (job.job_class === "cleanup_share") {
    await cleanShare(env, job, now);
    return;
  }
  if (job.job_class === "cleanup_object") {
    await cleanObject(env, job, now);
    return;
  }
  if (job.job_class === "reconcile_upload") {
    await reconcileUpload(env, job, now);
    return;
  }
  await compactChanges(env, job, now);
}

export async function processMaintenanceJob(
  env: Env,
  jobId: string,
  now = Date.now(),
): Promise<void> {
  const job = await claimJob(env, jobId, now, now + JOB_LEASE_MS);
  if (job === null) return;
  try {
    await processClaimedJob(env, job, now);
    logMaintenance(env, job.job_class, 200);
  } catch (error) {
    const code =
      error instanceof ApiProblem ? error.code : "dependency_unavailable";
    await retryLater(env, job, now, code);
    logMaintenance(env, job.job_class, 503, code);
  }
}

async function processDueJobsInOrder(
  env: Env,
  jobIds: readonly string[],
  now: number,
  index = 0,
): Promise<void> {
  const jobId = jobIds[index];
  if (jobId === undefined) return;
  await processMaintenanceJob(env, jobId, now);
  return processDueJobsInOrder(env, jobIds, now, index + 1);
}

async function expireUploads(env: Env, now: number): Promise<void> {
  const expired = await env.DB.prepare(
    "SELECT upload_id, vault_id FROM upload_sessions WHERE state = 'active' AND expires_at <= ? ORDER BY expires_at LIMIT ?",
  )
    .bind(now, MAINTENANCE_PAGE_SIZE)
    .all<{ upload_id: string; vault_id: string }>();
  const statements: D1PreparedStatement[] = [];
  for (const session of expired.results) {
    const jobId = crypto.randomUUID().replaceAll("-", "");
    statements.push(
      env.DB.prepare(
        "UPDATE upload_sessions SET state = 'expired', updated_at = ? WHERE upload_id = ? AND state = 'active' AND expires_at <= ?",
      ).bind(now, session.upload_id, now),
      env.DB.prepare(
        "UPDATE vaults SET reserved_bytes = reserved_bytes - (SELECT reserved_bytes FROM upload_sessions WHERE upload_id = ?), used_bytes = used_bytes + (SELECT reserved_bytes FROM upload_sessions WHERE upload_id = ?) WHERE vault_id = ? AND changes() = 1",
      ).bind(session.upload_id, session.upload_id, session.vault_id),
      env.DB.prepare(
        "INSERT INTO maintenance_jobs (job_id, vault_id, job_class, target_id, payload_json, state, next_attempt_at, created_at, updated_at) SELECT ?, ?, 'cleanup_upload', ?, '{}', 'queued', ?, ?, ? WHERE EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'expired')",
      ).bind(
        jobId,
        session.vault_id,
        session.upload_id,
        now,
        now,
        now,
        session.upload_id,
      ),
    );
  }
  if (statements.length > 0) await env.DB.batch(statements);
}

async function expireShares(env: Env, now: number): Promise<void> {
  const expired = await env.DB.prepare(
    "SELECT vault_id, share_id, state, upload_id FROM shares WHERE state IN ('pending', 'active') AND expires_at <= ? ORDER BY expires_at LIMIT ?",
  )
    .bind(now, MAINTENANCE_PAGE_SIZE)
    .all<{
      share_id: string;
      state: "pending" | "active";
      upload_id: string;
      vault_id: string;
    }>();
  const statements: D1PreparedStatement[] = [];
  for (const share of expired.results) {
    const jobId = crypto.randomUUID().replaceAll("-", "");
    if (share.state === "pending") {
      statements.push(
        env.DB.prepare(
          "UPDATE shares SET state = 'expired' WHERE vault_id = ? AND share_id = ? AND state = 'pending'",
        ).bind(share.vault_id, share.share_id),
        env.DB.prepare(
          "UPDATE upload_sessions SET state = 'expired', updated_at = ? WHERE upload_id = ? AND state = 'active'",
        ).bind(now, share.upload_id),
        env.DB.prepare(
          "UPDATE vaults SET reserved_bytes = reserved_bytes - (SELECT reserved_bytes FROM upload_sessions WHERE upload_id = ?), used_bytes = used_bytes + (SELECT reserved_bytes FROM upload_sessions WHERE upload_id = ?) WHERE vault_id = ? AND changes() = 1",
        ).bind(share.upload_id, share.upload_id, share.vault_id),
        env.DB.prepare(
          "INSERT INTO maintenance_jobs (job_id, vault_id, job_class, target_id, payload_json, state, next_attempt_at, created_at, updated_at) VALUES (?, ?, 'cleanup_share', ?, '{}', 'queued', ?, ?, ?)",
        ).bind(jobId, share.vault_id, share.share_id, now, now, now),
      );
      continue;
    }
    statements.push(
      env.DB.prepare(
        "UPDATE shares SET state = 'expired' WHERE vault_id = ? AND share_id = ? AND state = 'active'",
      ).bind(share.vault_id, share.share_id),
      env.DB.prepare(
        "INSERT INTO maintenance_jobs (job_id, vault_id, job_class, target_id, payload_json, state, next_attempt_at, created_at, updated_at) VALUES (?, ?, 'cleanup_share', ?, '{}', 'queued', ?, ?, ?)",
      ).bind(jobId, share.vault_id, share.share_id, now, now, now),
    );
  }
  if (statements.length > 0) await env.DB.batch(statements);
}

async function purgeExpiredMutationNonces(
  env: Env,
  now: number,
): Promise<void> {
  await env.DB.prepare(
    "DELETE FROM mutation_nonces WHERE rowid IN (SELECT rowid FROM mutation_nonces WHERE expires_at <= ? ORDER BY expires_at LIMIT ?)",
  )
    .bind(now, MAINTENANCE_PAGE_SIZE)
    .run();
}

async function scheduleChangeCompaction(env: Env, now: number): Promise<void> {
  const candidates = await env.DB.prepare(
    "SELECT changes.vault_id FROM changes WHERE changes.created_at < ? AND NOT EXISTS (SELECT 1 FROM maintenance_jobs WHERE maintenance_jobs.vault_id = changes.vault_id AND maintenance_jobs.job_class = 'compact_changes' AND maintenance_jobs.state IN ('queued', 'running')) GROUP BY changes.vault_id ORDER BY MIN(changes.created_at), changes.vault_id LIMIT ?",
  )
    .bind(now - CHANGE_RETENTION_MS, MAINTENANCE_PAGE_SIZE)
    .all<{ vault_id: string }>();
  const statements = candidates.results.map((vault) =>
    env.DB.prepare(
      "INSERT OR IGNORE INTO maintenance_jobs (job_id, vault_id, job_class, target_id, payload_json, state, next_attempt_at, created_at, updated_at) VALUES (?, ?, 'compact_changes', ?, '{}', 'queued', ?, ?, ?)",
    ).bind(
      crypto.randomUUID().replaceAll("-", ""),
      vault.vault_id,
      vault.vault_id,
      now,
      now,
      now,
    ),
  );
  if (statements.length > 0) await env.DB.batch(statements);
}

export async function runMaintenance(
  env: Env,
  now = Date.now(),
): Promise<void> {
  await expireUploads(env, now);
  await expireShares(env, now);
  await purgeExpiredMutationNonces(env, now);
  await scheduleChangeCompaction(env, now);
  const jobs = await dueJobIds(env, now, MAINTENANCE_PAGE_SIZE);
  await processDueJobsInOrder(env, jobs, now);
}

export async function maintenanceQueueMessage(
  env: Env,
  message: { job_id: string },
): Promise<void> {
  await processMaintenanceJob(env, message.job_id);
}
