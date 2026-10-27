import { env, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import { r2ChunkKey } from "../src/db";
import {
  bootstrap,
  createUploadPayload,
  jsonBody,
  makeDevice,
  signedFetch,
  signedRequest,
  testId,
  tombstonePayload,
  uploadAllChunks,
  type Device,
} from "./helpers";

async function committedObject(device: Device) {
  const payload = await createUploadPayload(device);
  const create = await signedFetch(device, {
    method: "POST",
    path: "/v1/uploads",
    body: JSON.stringify(payload.json),
    idempotencyKey: testId("create_operation"),
  });
  expect(create.status).toBe(201);
  const uploadId = String(payload.json.uploadId);
  await uploadAllChunks(device, uploadId, payload.chunks);
  const commit = await signedFetch(device, {
    method: "POST",
    path: `/v1/uploads/${uploadId}/commit`,
    body: JSON.stringify({ version: 1 }),
    idempotencyKey: testId("commit_operation"),
  });
  expect(commit.status).toBe(200);
  return {
    payload,
    uploadId,
    objectId: String(payload.json.objectId),
    revisionId: String(payload.json.revisionId),
  };
}

describe("Sona companion API", () => {
  it("bootstraps once, exposes authenticated capabilities, and rejects clock skew", async () => {
    const device = await makeDevice();
    const first = await bootstrap(device, "bootstrap_response_loss");
    expect(first.status).toBe(201);
    const replay = await bootstrap(device, "bootstrap_response_loss");
    expect(replay.status).toBe(201);
    expect(await jsonBody(replay)).toEqual(await jsonBody(first));
    const secondDevice = await makeDevice();
    const secondBootstrap = await bootstrap(
      secondDevice,
      "second_bootstrap_attempt",
    );
    expect(secondBootstrap.status).toBe(401);
    expect((await jsonBody(secondBootstrap)).code).toBe("unauthorized");

    const capabilities = await signedFetch(device, {
      method: "GET",
      path: "/v1/capabilities",
    });
    expect(capabilities.status).toBe(200);
    expect(capabilities.headers.get("cache-control")).toBe("no-store");

    const clockSkew = await signedFetch(device, {
      method: "GET",
      path: "/v1/capabilities",
      timestamp: Date.now() - 6 * 60 * 1000,
    });
    expect(clockSkew.status).toBe(401);
    expect((await jsonBody(clockSkew)).code).toBe("clock_skew");
    expect(clockSkew.headers.get("date")).not.toBeNull();
  });

  it("rejects request replay and idempotency body substitution", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const payload = await createUploadPayload(device);
    const body = JSON.stringify(payload.json);
    const idempotencyKey = testId("create_operation");
    const request = await signedRequest(device, {
      method: "POST",
      path: "/v1/uploads",
      body,
      idempotencyKey,
    });
    const first = await SELF.fetch(request.url, {
      method: request.method,
      headers: request.headers,
      body: await request.clone().arrayBuffer(),
    });
    expect(first.status).toBe(201);
    const replay = await SELF.fetch(request.url, {
      method: request.method,
      headers: request.headers,
      body: await request.clone().arrayBuffer(),
    });
    expect(replay.status).toBe(409);
    expect((await jsonBody(replay)).code).toBe("replay");

    const altered = JSON.stringify((await createUploadPayload(device)).json);
    const conflict = await signedFetch(device, {
      method: "POST",
      path: "/v1/uploads",
      body: altered,
      idempotencyKey,
    });
    expect(conflict.status).toBe(409);
    expect((await jsonBody(conflict)).code).toBe("idempotency_conflict");
  });

  it("commits immutable revisions and returns the stored result after response loss", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const payload = await createUploadPayload(device);
    const create = await signedFetch(device, {
      method: "POST",
      path: "/v1/uploads",
      body: JSON.stringify(payload.json),
      idempotencyKey: testId("create_operation"),
    });
    expect(create.status).toBe(201);
    const uploadId = String(payload.json.uploadId);
    await uploadAllChunks(device, uploadId, payload.chunks);
    const commitBody = JSON.stringify({ version: 1 });
    const commitKey = testId("commit_response_loss");
    const committed = await signedFetch(device, {
      method: "POST",
      path: `/v1/uploads/${uploadId}/commit`,
      body: commitBody,
      idempotencyKey: commitKey,
    });
    expect(committed.status).toBe(200);
    const replay = await signedFetch(device, {
      method: "POST",
      path: `/v1/uploads/${uploadId}/commit`,
      body: commitBody,
      idempotencyKey: commitKey,
    });
    expect(replay.status).toBe(200);
    expect(await jsonBody(replay)).toEqual(await jsonBody(committed));

    const manifest = await signedFetch(device, {
      method: "GET",
      path: `/v1/objects/${payload.json.objectId}/revisions/${payload.json.revisionId}/manifest`,
    });
    expect(manifest.status).toBe(200);
    const chunk = await signedFetch(device, {
      method: "GET",
      path: `/v1/objects/${payload.json.objectId}/revisions/${payload.json.revisionId}/chunks/0`,
    });
    expect(chunk.status).toBe(200);
    expect(new Uint8Array(await chunk.arrayBuffer())).toEqual(
      payload.chunks[0],
    );
  });

  it("allows one simultaneous compare-and-swap winner", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const objectId = testId("object");
    const first = await createUploadPayload(device, { objectId });
    const second = await createUploadPayload(device, { objectId });
    for (const payload of [first, second]) {
      const created = await signedFetch(device, {
        method: "POST",
        path: "/v1/uploads",
        body: JSON.stringify(payload.json),
        idempotencyKey: testId("create_operation"),
      });
      expect(created.status).toBe(201);
      await uploadAllChunks(
        device,
        String(payload.json.uploadId),
        payload.chunks,
      );
    }
    const responses = await Promise.all(
      [first, second].map((payload) =>
        signedFetch(device, {
          method: "POST",
          path: `/v1/uploads/${payload.json.uploadId}/commit`,
          body: JSON.stringify({ version: 1 }),
          idempotencyKey: testId("commit_operation"),
        }),
      ),
    );
    expect(responses.map((response) => response.status).sort()).toEqual([
      200, 409,
    ]);
    const headCount = await env.DB.prepare(
      "SELECT COUNT(*) AS count FROM object_heads WHERE vault_id = ? AND object_id = ?",
    )
      .bind(device.vaultId, objectId)
      .first<{ count: number }>();
    expect(headCount?.count).toBe(1);
    const changeCount = await env.DB.prepare(
      "SELECT COUNT(*) AS count FROM changes WHERE vault_id = ? AND object_id = ?",
    )
      .bind(device.vaultId, objectId)
      .first<{ count: number }>();
    expect(changeCount?.count).toBe(1);
  });

  it("keeps a tombstone head and blocks stale resurrection", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const committed = await committedObject(device);
    const stale = await createUploadPayload(device, {
      objectId: committed.objectId,
      baseRevisionId: committed.revisionId,
    });
    const staleCreate = await signedFetch(device, {
      method: "POST",
      path: "/v1/uploads",
      body: JSON.stringify(stale.json),
      idempotencyKey: testId("stale_create"),
    });
    expect(staleCreate.status).toBe(201);
    await uploadAllChunks(device, String(stale.json.uploadId), stale.chunks);
    const tombstone = await signedFetch(device, {
      method: "DELETE",
      path: `/v1/objects/${committed.objectId}`,
      body: await tombstonePayload(
        device,
        committed.objectId,
        committed.revisionId,
      ),
      idempotencyKey: testId("tombstone"),
    });
    expect(tombstone.status).toBe(200);
    const staleCommit = await signedFetch(device, {
      method: "POST",
      path: `/v1/uploads/${stale.json.uploadId}/commit`,
      body: JSON.stringify({ version: 1 }),
      idempotencyKey: testId("stale_commit"),
    });
    expect(staleCommit.status).toBe(409);
    expect((await jsonBody(staleCommit)).code).toBe("stale_revision");
    const oldRead = await signedFetch(device, {
      method: "GET",
      path: `/v1/objects/${committed.objectId}/revisions/${committed.revisionId}/manifest`,
    });
    expect(oldRead.status).toBe(404);
  });

  it("refuses to commit when an accepted receipt no longer has its R2 object", async () => {
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
    await env.CIPHERTEXT.delete(r2ChunkKey(device.vaultId, uploadId, 0));

    const commit = await signedFetch(device, {
      method: "POST",
      path: `/v1/uploads/${uploadId}/commit`,
      body: JSON.stringify({ version: 1 }),
      idempotencyKey: testId("commit_operation"),
    });
    expect(commit.status).toBe(422);
    expect((await jsonBody(commit)).code).toBe("integrity_failed");
    const heads = await env.DB.prepare(
      "SELECT COUNT(*) AS count FROM object_heads WHERE vault_id = ?",
    )
      .bind(device.vaultId)
      .first<{ count: number }>();
    const session = await env.DB.prepare(
      "SELECT state FROM upload_sessions WHERE upload_id = ?",
    )
      .bind(uploadId)
      .first<{ state: string }>();
    expect(heads?.count).toBe(0);
    expect(session?.state).toBe("active");
  });
});
