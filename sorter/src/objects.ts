import { z } from "zod";
import {
  canonicalUploadEnvelopeBytes,
  decryptObjectRevisionPayload,
  encryptObjectRevisionPayload,
} from "../../cloudflare/sona-companion/src/crypto";
import {
  base64UrlDecode,
  base64UrlEncode,
  decodeUtf8,
  randomId,
  sha256Base64Url,
  utf8,
} from "../../cloudflare/sona-companion/src/encoding";
import type { DeviceIdentity, ObjectUploadPlan, RevisionManifest } from "./client";
import { signEd25519 } from "./keys";

/* Source formats are bound into every payload's HKDF info and AES-GCM AAD, so a
 * manifest opens under exactly one of them; that is how a reader tells the kinds
 * apart without trusting the Worker. */
export const THOUGHT_SOURCE_FORMAT = "sona-thought-v1";
export const CARD_SOURCE_FORMAT = "sona-card-v1";

const count = z.number().int().nonnegative();

const ThoughtAudio = z.object({
  byte_length: count,
  channels: count,
  chunk_count: count,
  chunk_start: count,
  codec: z.string(),
  duration_ms: count,
  sample_rate_hz: count,
  sha256: z.string(),
});

const ThoughtImage = z.object({
  byte_length: count,
  chunk_count: count,
  chunk_start: count,
  height: count,
  mime: z.string(),
  sha256: z.string(),
  width: count,
});

/** What the phone captures: the transcript or typed text, plus attachments. */
export const ThoughtManifest = z.object({
  audio: ThoughtAudio.nullable(),
  captured_at_utc_ms: count,
  device_id: z.string(),
  format_version: z.literal(1),
  images: z.array(ThoughtImage),
  kind: z.literal("thought"),
  link: z.object({ url: z.string().url() }).nullable(),
  origin: z.enum(["voice", "typed", "shared"]),
  text: z.string(),
});
export type ThoughtManifest = z.infer<typeof ThoughtManifest>;

export const Cluster = z.object({
  hue: z.number().int().min(0).max(359),
  key: z.string().min(1),
  name: z.string().min(1),
});
export type Cluster = z.infer<typeof Cluster>;

/** What the sorter derives from one thought and the phone shows on the board. */
export const CardManifest = z.object({
  archived: z.boolean(),
  cluster: Cluster,
  format_version: z.literal(1),
  kind: z.literal("card"),
  pinned: z.boolean(),
  sorter: z.object({ model: z.string(), prompt_version: count }).nullable(),
  summary: z.string(),
  tags: z.array(z.string()),
  thought_id: z.string(),
  thought_revision_id: z.string(),
  title: z.string(),
  writer: z.object({ device_id: z.string(), role: z.enum(["sorter", "user"]) }),
  written_at_utc_ms: count,
});
export type CardManifest = z.infer<typeof CardManifest>;

export type OpenedObject =
  | { kind: "card"; manifest: CardManifest }
  | { kind: "other" }
  | { kind: "thought"; manifest: ThoughtManifest };

type Json = { [key: string]: Json } | Json[] | boolean | number | string | null;

/** A stable hue for a cluster key: FNV-1a over the key, folded onto the colour wheel. */
export function clusterHue(key: string): number {
  let hash = 0x811c9dc5;
  for (const byte of utf8(key)) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash % 360;
}

/** JSON with object keys sorted at every depth, so one manifest has one byte form. */
export function canonicalJson(value: Json): Uint8Array {
  const sorted = (node: Json): Json => {
    if (Array.isArray(node)) return node.map(sorted);
    if (node !== null && typeof node === "object") {
      const out: { [key: string]: Json } = {};
      for (const [key, child] of Object.entries(node).sort(([a], [b]) => (a < b ? -1 : 1)))
        out[key] = sorted(child);
      return out;
    }
    return node;
  };
  return utf8(JSON.stringify(sorted(value)));
}

export interface VaultKeys {
  vaultId: string;
  vaultRoot: Uint8Array;
}

/**
 * Open a revision manifest as a thought, a card, or something else in the vault.
 *
 * The Worker's word is checked only where it costs nothing: the manifest digest
 * matches the envelope, and the crypto version is the one this reader speaks.
 * Plaintext authenticity comes from the AEAD, which binds the bytes to this vault,
 * object, revision and format; the writer signature would add only device
 * attribution, which no card uses, and verifying it means downloading every chunk.
 */
export async function openManifest(
  keys: VaultKeys,
  revision: RevisionManifest,
): Promise<OpenedObject> {
  const { envelope } = revision;
  const ciphertext = base64UrlDecode(revision.manifest);
  if (
    ciphertext === null ||
    envelope.crypto_version !== 1 ||
    (await sha256Base64Url(ciphertext)) !== envelope.manifest_sha256
  ) {
    throw new Error(`manifest ${envelope.object_id}/${envelope.revision_id} failed integrity`);
  }
  const open = (sourceFormat: string): Promise<Uint8Array | null> =>
    decryptObjectRevisionPayload({
      contentKind: "manifest",
      index: 0,
      objectId: envelope.object_id,
      revisionId: envelope.revision_id,
      sourceFormat,
      total: envelope.chunk_count,
      vaultId: keys.vaultId,
      vaultRoot: keys.vaultRoot,
      ciphertext,
    }).catch(() => null);
  const thought = await open(THOUGHT_SOURCE_FORMAT);
  if (thought !== null) {
    return { kind: "thought", manifest: ThoughtManifest.parse(JSON.parse(decodeUtf8(thought))) };
  }
  const card = await open(CARD_SOURCE_FORMAT);
  if (card !== null) {
    return { kind: "card", manifest: CardManifest.parse(JSON.parse(decodeUtf8(card))) };
  }
  return { kind: "other" };
}

export interface SealedCard {
  chunk: Uint8Array;
  chunkDigest: string;
  plan: ObjectUploadPlan;
}

/**
 * Seal a new card as a fresh object: manifest plus the one empty chunk crypto v1
 * requires, and the upload plan the Worker verifies against this device's key.
 */
export async function sealCard(
  keys: VaultKeys,
  identity: DeviceIdentity,
  card: CardManifest,
): Promise<SealedCard> {
  const objectId = randomId();
  const revisionId = randomId();
  const context = {
    objectId,
    revisionId,
    sourceFormat: CARD_SOURCE_FORMAT,
    total: 1,
    vaultId: keys.vaultId,
    vaultRoot: keys.vaultRoot,
  };
  const manifest = await encryptObjectRevisionPayload({
    ...context,
    contentKind: "manifest",
    index: 0,
    nonce: crypto.getRandomValues(new Uint8Array(12)),
    plaintext: canonicalJson(card),
  });
  const chunk = await encryptObjectRevisionPayload({
    ...context,
    contentKind: "chunk",
    index: 0,
    nonce: crypto.getRandomValues(new Uint8Array(12)),
    plaintext: new Uint8Array(),
  });
  const chunkDigest = await sha256Base64Url(chunk);
  const manifestSha256 = await sha256Base64Url(manifest);
  const chunks = [{ index: 0, sha256: chunkDigest, size: chunk.length }];
  const writerSignature = await signEd25519(
    identity.signingSeed,
    canonicalUploadEnvelopeBytes({
      baseRevisionId: null,
      chunks,
      cryptoVersion: 1,
      kind: "object",
      manifestDigest: manifestSha256,
      objectId,
      revisionId,
      shareId: null,
      totalBytes: chunk.length,
      vaultId: keys.vaultId,
    }),
  );
  return {
    chunk,
    chunkDigest,
    plan: {
      baseRevisionId: null,
      chunkCount: 1,
      chunks,
      cryptoVersion: 1,
      manifest: base64UrlEncode(manifest),
      manifestSha256,
      objectId,
      revisionId,
      totalBytes: chunk.length,
      uploadId: randomId(),
      version: 1,
      writerSignature: base64UrlEncode(writerSignature),
    },
  };
}

/** Stable idempotency key: base64url(sha256(part || 0x00 …)), as `runtime.rs` derives it. */
export async function idempotencyKey(parts: readonly string[]): Promise<string> {
  const bytes: number[] = [];
  for (const part of parts) bytes.push(...utf8(part), 0);
  return sha256Base64Url(new Uint8Array(bytes));
}
