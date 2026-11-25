// The sorter runs unattended for weeks on a VPS: a clock that drifts past the
// Worker's skew window, or one request that never answers, stops filing for good.
import { afterEach, expect, jest, test } from "bun:test";
import type { ThoughtManifest } from "../src/objects";
import { ApiError, CompanionClient } from "../src/client";
import { classify } from "../src/sort";

const IDENTITY = {
  deviceId: "sorter_device_01",
  signingSeed: new Uint8Array(32).fill(9),
};

const VAULT_ID = "fixture_vault_0001";

const THOUGHT: ThoughtManifest = {
  audio: null,
  captured_at_utc_ms: 1_700_000_000_000,
  device_id: "phone_device_01",
  format_version: 1,
  images: [],
  kind: "thought",
  link: null,
  origin: "typed",
  text: "Pinch to zoom the board",
};

const MINUTE_MS = 60 * 1000;

let server: Bun.Server<undefined> | null = null;

afterEach(() => {
  jest.useRealTimers();
  server?.stop(true);
  server = null;
});

/** A server that takes every request and never answers it. */
function stalledServer() {
  const { promise, resolve } = Promise.withResolvers<void>();
  server = Bun.serve({
    port: 0,
    fetch: () => {
      resolve();
      return Promise.withResolvers<Response>().promise;
    },
  });
  return { received: promise, url: `http://localhost:${server.port}` };
}

test("a Worker reply's Date moves the signing clock, so drift after boot is followed", async () => {
  let serverAheadMs = 10 * MINUTE_MS;
  server = Bun.serve({
    port: 0,
    fetch: () =>
      Response.json(
        { code: "clock_skew" },
        {
          headers: { date: new Date(Date.now() + serverAheadMs).toUTCString() },
          status: 401,
        },
      ),
  });
  const client = new CompanionClient(
    `http://localhost:${server.port}`,
    IDENTITY,
    VAULT_ID,
  );
  await client.syncClock();
  expect(client.nowUtcMs() - Date.now()).toBeGreaterThan(9 * MINUTE_MS);

  serverAheadMs = 20 * MINUTE_MS;
  await expect(client.selfDevice()).rejects.toBeInstanceOf(ApiError);
  expect(client.nowUtcMs() - Date.now()).toBeGreaterThan(19 * MINUTE_MS);
});

test("a Worker request that never answers fails instead of holding the loop", async () => {
  const { received, url } = stalledServer();
  const client = new CompanionClient(url, IDENTITY, VAULT_ID);
  jest.useFakeTimers();
  const reply = client.selfDevice();
  await received;
  jest.advanceTimersByTime(10 * MINUTE_MS);
  await expect(reply).rejects.toMatchObject({ name: "TimeoutError" });
});

test("a model request that never answers fails instead of holding the loop", async () => {
  const { received, url } = stalledServer();
  jest.useFakeTimers();
  const reply = classify(
    { apiKey: "test-key", baseUrl: url, model: "test-model" },
    THOUGHT,
    [],
  );
  await received;
  jest.advanceTimersByTime(10 * MINUTE_MS);
  await expect(reply).rejects.toMatchObject({ name: "TimeoutError" });
});
