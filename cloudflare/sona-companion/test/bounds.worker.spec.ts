import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import {
  bootstrap,
  createUploadPayload,
  jsonBody,
  makeDevice,
  signedFetch,
  testId,
} from "./helpers";

describe("bounded upload behavior", () => {
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
});
