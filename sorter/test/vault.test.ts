// A card the index takes for `other` leaves its thought unfiled, and the next
// pass files that thought again as a second card. A commit whose reply was lost
// is found only through the change feed, so a manifest read that fails in
// transit must fail the pass instead of indexing the card as `other`.
import { afterEach, expect, test } from "bun:test";
import { z } from "zod";
import { mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { encryptObjectRevisionPayload } from "../../cloudflare/sona-companion/src/crypto";
import {
  base64UrlEncode,
  sha256Base64Url,
} from "../../cloudflare/sona-companion/src/encoding";
import { ApiError, CompanionClient } from "../src/client";
import {
  type CardManifest,
  THOUGHT_SOURCE_FORMAT,
  type VaultKeys,
  canonicalJson,
  cardObjectId,
  sealCard,
} from "../src/objects";
import { StateDir } from "../src/state";
import { Sorter } from "../src/vault";

const KEYS: VaultKeys = {
  vaultId: "fixture_vault_0001",
  vaultRoot: new Uint8Array(32).fill(7),
};

const IDENTITY = {
  deviceId: "sorter_device_01",
  signingSeed: new Uint8Array(32).fill(9),
};

const CARD: CardManifest = {
  archived: false,
  cluster: { hue: 212, key: "side-projects", name: "Side Projects" },
  format_version: 1,
  kind: "card",
  pinned: false,
  sorter: { model: "test-model", prompt_version: 1 },
  summary: "Build a tiny board for brainstorms.",
  tags: ["sona", "board"],
  thought_id: "thought_01",
  thought_revision_id: "thought_rev_01",
  title: "Brainstorm board",
  writer: { device_id: IDENTITY.deviceId, role: "sorter" },
  written_at_utc_ms: 1_700_000_000_000,
};

let server: Bun.Server<undefined> | null = null;
let root: string | null = null;

afterEach(async () => {
  server?.stop(true);
  server = null;
  if (root !== null) await rm(root, { force: true, recursive: true });
  root = null;
});

test("a manifest read that fails in transit leaves the card unread, not indexed as other", async () => {
  const { plan } = await sealCard(KEYS, IDENTITY, CARD);
  let manifestReads = 0;
  server = Bun.serve({
    port: 0,
    fetch: (request) => {
      const path = new URL(request.url).pathname;
      if (path === "/v1/changes") {
        return Response.json({
          changes: [
            {
              object_id: plan.objectId,
              revision_id: plan.revisionId,
              sequence: 1,
              tombstone: false,
            },
          ],
          has_more: false,
          high_water: 1,
          next_cursor: "cursor_after_1",
        });
      }
      manifestReads += 1;
      if (manifestReads === 1) {
        return Response.json(
          { code: "dependency_unavailable", retryable: true },
          { status: 503 },
        );
      }
      return Response.json({
        envelope: {
          chunk_count: plan.chunkCount,
          crypto_version: plan.cryptoVersion,
          manifest_sha256: plan.manifestSha256,
          object_id: plan.objectId,
          parent_revision_id: plan.baseRevisionId,
          revision_id: plan.revisionId,
          total_bytes: plan.totalBytes,
          writer_device_id: IDENTITY.deviceId,
          writer_signature: plan.writerSignature,
        },
        manifest: plan.manifest,
      });
    },
  });
  root = await mkdtemp(join("/private/var/tmp", "sona-sorter-test-"));
  const state = new StateDir(root);
  const sorter = new Sorter({
    client: new CompanionClient(
      `http://localhost:${server.port}`,
      IDENTITY,
      KEYS.vaultId,
    ),
    identity: IDENTITY,
    keys: KEYS,
    log: () => undefined,
    model: { apiKey: "test-key", baseUrl: "http://localhost:9", model: "m" },
    state,
  });

  await expect(sorter.pass()).rejects.toBeInstanceOf(ApiError);
  expect(await sorter.pass()).toStrictEqual({ sorted: 0, synced: 1 });
  expect((await state.feed()).objects[plan.objectId]).toStrictEqual({
    cluster: { key: "side-projects", name: "Side Projects" },
    kind: "card",
    revision_id: plan.revisionId,
    thought_id: "thought_01",
  });
});

// The Mac's sorter files a thought under the same card object id. When it
// commits first, this sorter's commit is refused as stale: that thought is
// filed, so it is neither counted nor retried.
test("a card another sorter committed first is taken as filed, not retried", async () => {
  const thoughtId = "thought_voice_01";
  const thoughtRevision = "thought_rev_voice_01";
  const plaintext = canonicalJson({
    audio: null,
    captured_at_utc_ms: 1_700_000_000_000,
    device_id: "phone_device_01",
    format_version: 1,
    images: [],
    kind: "thought",
    link: null,
    origin: "voice",
    text: "",
  });
  const manifest = await encryptObjectRevisionPayload({
    contentKind: "manifest",
    index: 0,
    nonce: new Uint8Array(12).fill(3),
    objectId: thoughtId,
    plaintext,
    revisionId: thoughtRevision,
    sourceFormat: THOUGHT_SOURCE_FORMAT,
    total: 1,
    vaultId: KEYS.vaultId,
    vaultRoot: KEYS.vaultRoot,
  });
  const committed: string[] = [];
  let cancellations = 0;
  server = Bun.serve({
    port: 0,
    fetch: async (request) => {
      const path = new URL(request.url).pathname;
      if (request.method === "DELETE") {
        cancellations += 1;
        return Response.json({
          state: "cancelled",
          upload_id: path.split("/")[3],
        });
      }
      if (path === "/v1/changes") {
        return Response.json({
          changes: [
            {
              object_id: thoughtId,
              revision_id: thoughtRevision,
              sequence: 1,
              tombstone: false,
            },
          ],
          has_more: false,
          high_water: 1,
          next_cursor: "cursor_after_1",
        });
      }
      if (path.endsWith("/manifest")) {
        return Response.json({
          envelope: {
            chunk_count: 1,
            crypto_version: 1,
            manifest_sha256: await sha256Base64Url(manifest),
            object_id: thoughtId,
            parent_revision_id: null,
            revision_id: thoughtRevision,
            total_bytes: 0,
            writer_device_id: "phone_device_01",
            writer_signature: "unused",
          },
          manifest: base64UrlEncode(manifest),
        });
      }
      if (path === "/v1/uploads") {
        const plan = z
          .object({
            objectId: z.string(),
            uploadId: z.string(),
          })
          .parse(await request.json());
        committed.push(plan.objectId);
        return Response.json({
          accepted_indexes: [],
          state: "active",
          upload_id: plan.uploadId,
        });
      }
      if (path.endsWith("/chunks/0")) {
        return Response.json({
          accepted: true,
          index: 0,
          upload_id: path.split("/")[3],
        });
      }
      return Response.json(
        { code: "stale_revision", retryable: false },
        { status: 409 },
      );
    },
  });
  root = await mkdtemp(join("/private/var/tmp", "sona-sorter-test-"));
  const state = new StateDir(root);
  const sorter = new Sorter({
    client: new CompanionClient(
      `http://localhost:${server.port}`,
      IDENTITY,
      KEYS.vaultId,
    ),
    identity: IDENTITY,
    keys: KEYS,
    log: () => undefined,
    model: { apiKey: "test-key", baseUrl: "http://localhost:9", model: "m" },
    state,
  });

  expect(await sorter.pass()).toStrictEqual({ sorted: 0, synced: 1 });
  expect(committed).toStrictEqual([await cardObjectId(thoughtId)]);
  expect(cancellations).toBe(1);
  expect((await state.feed()).attempts).toStrictEqual({});
});

test("a thought's card object id is the one the Mac derives", async () => {
  expect(await cardObjectId("thought_01")).toBe(
    "rr5feVJMsR91jsgmiCzMXec5MRKmVyEs7KmBTQwyx4M",
  );
});
