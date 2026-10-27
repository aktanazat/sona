import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import {
  CHANGE_RETENTION_MS,
  JOB_RETRY_MS,
  MAINTENANCE_PAGE_SIZE,
  MAX_CHUNK_BYTES,
} from "../src/constants";
import { claimJob, r2ChunkKey } from "../src/db";
import { base64UrlDecode, sha256Base64Url } from "../src/encoding";
import { runMaintenance } from "../src/maintenance";
import {
  bootstrap,
  createUploadPayload,
  jsonBody,
  makeDevice,
  signedFetch,
  testId,
  uploadAllChunks,
} from "./helpers";

describe("maintenance", () => {
  it("continues reconciliation after a bounded scan page", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const chunks = Array.from(
      { length: MAINTENANCE_PAGE_SIZE + 1 },
      (_, index) =>
        new Uint8Array(index < MAINTENANCE_PAGE_SIZE ? MAX_CHUNK_BYTES : 28),
    );
    const payload = await createUploadPayload(device, { chunks });
    const created = await signedFetch(device, {
      method: "POST",
      path: "/v1/uploads",
      body: JSON.stringify(payload.json),
      idempotencyKey: testId("create_operation"),
    });
    expect(created.status).toBe(201);

    const uploadId = String(payload.json.uploadId);
    for (const [index, chunk] of chunks.entries()) {
      const digest = await sha256Base64Url(chunk);
      const digestBytes = base64UrlDecode(digest);
      if (digestBytes === null) throw new Error("test digest decode failed");
      await env.CIPHERTEXT.put(
        r2ChunkKey(device.vaultId, uploadId, index),
        chunk,
        {
          customMetadata: { sha256: digest },
          sha256: digestBytes,
        },
      );
    }

    const now = Date.now();
    await runMaintenance(env, now);
    const afterFirstPage = await env.DB.prepare(
      "SELECT progress, state FROM maintenance_jobs WHERE vault_id = ? AND job_class = 'reconcile_upload' AND target_id = ?",
    )
      .bind(device.vaultId, uploadId)
      .first<{ progress: number; state: string }>();
    expect(afterFirstPage).toEqual({
      progress: MAINTENANCE_PAGE_SIZE,
      state: "queued",
    });

    await runMaintenance(env, now + JOB_RETRY_MS);
    const status = await signedFetch(device, {
      method: "GET",
      path: "/v1/uploads/" + uploadId,
    });
    expect(status.status).toBe(200);
    expect((await jsonBody(status)).accepted_indexes).toEqual(
      chunks.map((_, index) => index),
    );
  });

  it("purges expired mutation nonces in a bounded maintenance pass", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const now = Date.now();
    await env.DB.prepare(
      "INSERT INTO mutation_nonces (vault_id, device_id, nonce, expires_at, created_at) VALUES (?, ?, ?, ?, ?)",
    )
      .bind(
        device.vaultId,
        device.deviceId,
        new Uint8Array(16),
        now - 1,
        now - 2,
      )
      .run();

    await runMaintenance(env, now);
    const remaining = await env.DB.prepare(
      "SELECT COUNT(*) AS count FROM mutation_nonces WHERE vault_id = ? AND device_id = ?",
    )
      .bind(device.vaultId, device.deviceId)
      .first<{ count: number }>();
    expect(remaining?.count).toBe(0);
  });

  it("reclaims expired jobs with a fresh lease token", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const now = Date.now();
    const jobId = testId("maintenance_job");
    await env.DB.prepare(
      "INSERT INTO maintenance_jobs (job_id, vault_id, job_class, target_id, payload_json, state, next_attempt_at, created_at, updated_at) VALUES (?, ?, 'compact_changes', ?, '{}', 'queued', ?, ?, ?)",
    )
      .bind(jobId, device.vaultId, device.vaultId, now, now, now)
      .run();

    const first = await claimJob(env, jobId, now, now + 1);
    const second = await claimJob(env, jobId, now + 2, now + 3);
    if (first === null || second === null)
      throw new Error("test job was not claimed");
    expect(second.lease_token).not.toBe(first.lease_token);

    const current = await env.DB.prepare(
      "SELECT lease_token, state FROM maintenance_jobs WHERE job_id = ?",
    )
      .bind(jobId)
      .first<{ lease_token: string; state: string }>();
    expect(current).toEqual({
      lease_token: second.lease_token,
      state: "running",
    });
  });

  it("compacts expired changes and makes stale cursors expire", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const payload = await createUploadPayload(device);
    const created = await signedFetch(device, {
      method: "POST",
      path: "/v1/uploads",
      body: JSON.stringify(payload.json),
      idempotencyKey: testId("create_operation"),
    });
    expect(created.status).toBe(201);
    const uploadId = String(payload.json.uploadId);
    await uploadAllChunks(device, uploadId, payload.chunks);
    const committed = await signedFetch(device, {
      method: "POST",
      path: "/v1/uploads/" + uploadId + "/commit",
      body: JSON.stringify({ version: 1 }),
      idempotencyKey: testId("commit_operation"),
    });
    expect(committed.status).toBe(200);

    const objectId = String(payload.json.objectId);
    const revisionId = String(payload.json.revisionId);
    const now = Date.now();
    const expiredAt = now - CHANGE_RETENTION_MS - 1;
    const statements = Array.from(
      { length: MAINTENANCE_PAGE_SIZE },
      (_, index) =>
        env.DB.prepare(
          "INSERT INTO changes (vault_id, sequence, object_id, revision_id, tombstone, created_at) VALUES (?, ?, ?, ?, 0, ?)",
        ).bind(device.vaultId, index + 2, objectId, revisionId, expiredAt),
    );
    await env.DB.batch([
      env.DB.prepare(
        "UPDATE vaults SET next_change_sequence = ? WHERE vault_id = ?",
      ).bind(MAINTENANCE_PAGE_SIZE + 1, device.vaultId),
      env.DB.prepare(
        "UPDATE changes SET created_at = ? WHERE vault_id = ?",
      ).bind(expiredAt, device.vaultId),
      ...statements,
    ]);
    await runMaintenance(env, now);
    const afterFirstPage = await env.DB.prepare(
      "SELECT progress, state FROM maintenance_jobs WHERE vault_id = ? AND job_class = 'compact_changes'",
    )
      .bind(device.vaultId)
      .first<{ progress: number; state: string }>();
    expect(afterFirstPage).toEqual({
      progress: MAINTENANCE_PAGE_SIZE,
      state: "queued",
    });
    const remainingAfterFirstPage = await env.DB.prepare(
      "SELECT COUNT(*) AS count FROM changes WHERE vault_id = ?",
    )
      .bind(device.vaultId)
      .first<{ count: number }>();
    expect(remainingAfterFirstPage?.count).toBe(1);

    const changes = await signedFetch(device, {
      method: "GET",
      path: "/v1/changes",
    });
    expect(changes.status).toBe(410);
    expect((await jsonBody(changes)).code).toBe("cursor_expired");

    await runMaintenance(env, now + 1);
    const afterSecondPage = await env.DB.prepare(
      "SELECT progress, state FROM maintenance_jobs WHERE vault_id = ? AND job_class = 'compact_changes'",
    )
      .bind(device.vaultId)
      .first<{ progress: number; state: string }>();
    expect(afterSecondPage).toEqual({
      progress: MAINTENANCE_PAGE_SIZE,
      state: "done",
    });
    const remainingAfterSecondPage = await env.DB.prepare(
      "SELECT COUNT(*) AS count FROM changes WHERE vault_id = ?",
    )
      .bind(device.vaultId)
      .first<{ count: number }>();
    expect(remainingAfterSecondPage?.count).toBe(0);
  });
});
