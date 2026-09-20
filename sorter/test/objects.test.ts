// A card the sorter cannot read back is a thought it files again on every pass,
// so the seal and open sides must agree on the revision context byte for byte.
import { expect, test } from "bun:test";
import type { RevisionManifest } from "../src/client";
import { type CardManifest, type VaultKeys, openManifest, sealCard } from "../src/objects";

const KEYS: VaultKeys = {
  vaultId: "fixture_vault_0001",
  vaultRoot: new Uint8Array(32).fill(7),
};

const IDENTITY = { deviceId: "sorter_device_01", signingSeed: new Uint8Array(32).fill(9) };

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

test("openManifest returns the card sealCard sealed, read through the Worker's reply shape", async () => {
  const sealed = await sealCard(KEYS, IDENTITY, CARD);
  const { plan } = sealed;
  const reply: RevisionManifest = {
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
  };
  expect(await openManifest(KEYS, reply)).toStrictEqual({ kind: "card", manifest: CARD });
});
