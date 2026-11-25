import type { MaintenanceClass } from "./constants";
import { base64UrlEncode, randomId, toBytes } from "./encoding";
import { problem } from "./errors";
import { isJsonArray, isJsonInteger, isJsonRecord, isJsonString } from "./json";
import type {
  ActiveDeviceRow,
  Env,
  IdempotencyRow,
  MaintenanceJobRow,
  ObjectHeadRow,
  ShareRow,
  UploadPlanChunk,
  UploadSessionRow,
} from "./types";

export function r2UploadPrefix(vaultId: string, uploadId: string): string {
  return `v1/${vaultId}/u/${uploadId}/c/`;
}

export function r2ChunkKey(
  vaultId: string,
  uploadId: string,
  index: number,
): string {
  return `${r2UploadPrefix(vaultId, uploadId)}${index}`;
}

export function changeCount(result: D1Result<unknown>): number {
  return result.meta.changes ?? 0;
}

export function batchResult(
  results: readonly D1Result<unknown>[],
  index: number,
): D1Result<unknown> {
  const result = results[index];
  if (result === undefined) throw problem("integrity_failed");
  return result;
}

export function planToJson(chunks: readonly UploadPlanChunk[]): string {
  return JSON.stringify(chunks);
}

export function planFromJson(source: string): UploadPlanChunk[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(source);
  } catch {
    throw problem("integrity_failed");
  }
  if (!isJsonArray(parsed)) throw problem("integrity_failed");
  const chunks: UploadPlanChunk[] = [];
  for (const value of parsed) {
    if (!isJsonRecord(value)) throw problem("integrity_failed");
    const index = value.index;
    const size = value.size;
    const sha256 = value.sha256;
    if (
      !isJsonInteger(index) ||
      !isJsonInteger(size) ||
      !isJsonString(sha256)
    ) {
      throw problem("integrity_failed");
    }
    chunks.push({ index, size, sha256 });
  }
  return chunks;
}

export async function activeDevice(
  env: Env,
  vaultId: string,
  deviceId: string,
): Promise<ActiveDeviceRow | null> {
  return env.DB.prepare(
    "SELECT device_id, signing_public_key, status FROM devices WHERE vault_id = ? AND device_id = ?",
  )
    .bind(vaultId, deviceId)
    .first<ActiveDeviceRow>();
}

export async function uploadSession(
  env: Env,
  vaultId: string,
  uploadId: string,
): Promise<UploadSessionRow | null> {
  return env.DB.prepare(
    "SELECT * FROM upload_sessions WHERE vault_id = ? AND upload_id = ?",
  )
    .bind(vaultId, uploadId)
    .first<UploadSessionRow>();
}

export async function objectHead(
  env: Env,
  vaultId: string,
  objectId: string,
): Promise<ObjectHeadRow | null> {
  return env.DB.prepare(
    "SELECT object_id, head_revision_id, head_sequence, tombstone FROM object_heads WHERE vault_id = ? AND object_id = ?",
  )
    .bind(vaultId, objectId)
    .first<ObjectHeadRow>();
}

export async function shareRow(
  env: Env,
  vaultId: string,
  shareId: string,
): Promise<ShareRow | null> {
  return env.DB.prepare(
    "SELECT * FROM shares WHERE vault_id = ? AND share_id = ?",
  )
    .bind(vaultId, shareId)
    .first<ShareRow>();
}

export async function idempotencyResult(
  env: Env,
  vaultId: string,
  deviceId: string,
  idempotencyKey: string,
): Promise<IdempotencyRow | null> {
  return env.DB.prepare(
    "SELECT operation, target, body_digest, status, response_json FROM idempotency_results WHERE vault_id = ? AND device_id = ? AND idempotency_key = ?",
  )
    .bind(vaultId, deviceId, idempotencyKey)
    .first<IdempotencyRow>();
}

export async function claimMutationNonce(
  env: Env,
  vaultId: string,
  deviceId: string,
  nonce: Uint8Array,
  now: number,
  expiresAt: number,
): Promise<void> {
  const result = await env.DB.prepare(
    "INSERT OR IGNORE INTO mutation_nonces (vault_id, device_id, nonce, expires_at, created_at) VALUES (?, ?, ?, ?, ?)",
  )
    .bind(vaultId, deviceId, nonce, expiresAt, now)
    .run();
  if (changeCount(result) === 0) throw problem("replay");
}

export function newMaintenanceJob(input: {
  jobClass: MaintenanceClass;
  now: number;
  payload: Record<string, string>;
  targetId: string;
  vaultId: string;
}) {
  return { ...input, jobId: randomId() };
}

export function scheduleMaintenance(
  env: Env,
  context: ExecutionContext,
  jobId: string,
): void {
  context.waitUntil(
    env.MAINTENANCE.send({ job_id: jobId }).catch(() => undefined),
  );
}

export async function claimJob(
  env: Env,
  jobId: string,
  now: number,
  leaseUntil: number,
): Promise<MaintenanceJobRow | null> {
  const leaseToken = randomId();
  const claim = await env.DB.prepare(
    "UPDATE maintenance_jobs SET state = 'running', lease_until = ?, lease_token = ?, attempt_count = attempt_count + 1, updated_at = ? WHERE job_id = ? AND ((state = 'queued' AND next_attempt_at <= ?) OR (state = 'running' AND lease_until <= ?))",
  )
    .bind(leaseUntil, leaseToken, now, jobId, now, now)
    .run();
  if (changeCount(claim) === 0) return null;
  return env.DB.prepare(
    "SELECT job_id, vault_id, job_class, target_id, payload_json, state, progress, attempt_count, lease_token FROM maintenance_jobs WHERE job_id = ? AND state = 'running' AND lease_token = ?",
  )
    .bind(jobId, leaseToken)
    .first<MaintenanceJobRow>();
}

export async function dueJobIds(
  env: Env,
  now: number,
  limit: number,
): Promise<string[]> {
  const result = await env.DB.prepare(
    "SELECT job_id FROM maintenance_jobs WHERE (state = 'queued' AND next_attempt_at <= ?) OR (state = 'running' AND lease_until <= ?) ORDER BY next_attempt_at, created_at LIMIT ?",
  )
    .bind(now, now, limit)
    .all<{ job_id: string }>();
  return result.results.map((row) => row.job_id);
}

export function bytesFromDb(value: ArrayBuffer): Uint8Array {
  return toBytes(value);
}

export function responseDigestHeader(value: string): string {
  return `"${value}"`;
}

export function deviceKeyText(value: ArrayBuffer): string {
  return base64UrlEncode(new Uint8Array(value));
}
