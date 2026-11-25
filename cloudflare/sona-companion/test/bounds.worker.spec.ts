import { env } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";

import { MAX_CHUNK_BYTES } from "../src/constants";
import {
  bootstrap,
  createUploadPayload,
  jsonBody,
  makeDevice,
  signedFetch,
  testId,
  uploadAllChunks,
  type Device,
} from "./helpers";

/* Every binding call is a subrequest, and a Worker invocation may make at most
 * 1,000 of them on the Free plan: a cost that grows per chunk caps the largest
 * upload a vault can commit or a share can serve. */
const CHUNKS = [
  new Uint8Array(MAX_CHUNK_BYTES),
  new Uint8Array(MAX_CHUNK_BYTES),
  new Uint8Array(28),
];

async function stagedUpload(
  device: Device,
  input: { shareId?: string } = {},
): Promise<string> {
  const payload = await createUploadPayload(device, {
    chunks: CHUNKS,
    ...input,
  });
  const created = await signedFetch(device, {
    method: "POST",
    path: input.shareId === undefined ? "/v1/uploads" : "/v1/shares",
    body: JSON.stringify(payload.json),
    idempotencyKey: testId("create_operation"),
  });
  expect(created.status).toBe(201);
  const uploadId = String(payload.json.uploadId);
  await uploadAllChunks(device, uploadId, payload.chunks);
  return uploadId;
}

describe("bounded upload behavior", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("rejects a non-final short chunk before creating an upload", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const payload = await createUploadPayload(device, {
      chunks: [new Uint8Array(28), new Uint8Array(28)],
    });
    const response = await signedFetch(device, {
      method: "POST",
      path: "/v1/uploads",
      body: JSON.stringify(payload.json),
      idempotencyKey: testId("fixed_chunk_boundary"),
    });

    expect(response.status).toBe(400);
    expect((await jsonBody(response)).code).toBe("invalid_request");
    const count = await env.DB.prepare(
      "SELECT COUNT(*) AS count FROM upload_sessions WHERE vault_id = ?",
    )
      .bind(device.vaultId)
      .first<{ count: number }>();
    expect(count?.count).toBe(0);
  });

  it("checks a commit's stored chunks with one listing, not one read per chunk", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const uploadId = await stagedUpload(device);

    const heads = vi.spyOn(env.CIPHERTEXT, "head");
    const lists = vi.spyOn(env.CIPHERTEXT, "list");
    const committed = await signedFetch(device, {
      method: "POST",
      path: `/v1/uploads/${uploadId}/commit`,
      body: JSON.stringify({ version: 1 }),
      idempotencyKey: testId("commit_operation"),
    });

    expect(committed.status).toBe(200);
    expect(heads).toHaveBeenCalledTimes(0);
    expect(lists).toHaveBeenCalledTimes(1);
  });
});
