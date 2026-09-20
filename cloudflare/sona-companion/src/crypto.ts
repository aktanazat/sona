import { concatBytes, u32, utf8 } from "./encoding";

export interface CanonicalRequestInput {
  audience: string;
  bodyDigest: string;
  contentType: string;
  deviceId: string;
  idempotencyKey: string;
  method: string;
  nonce: Uint8Array;
  path: string;
  query: readonly [string, string][];
  timestamp: number;
  vaultId: string;
}

export interface CanonicalUploadEnvelopeInput {
  baseRevisionId: string | null;
  chunks: readonly { index: number; sha256: string; size: number }[];
  cryptoVersion: number;
  kind: "object" | "share";
  manifestDigest: string;
  objectId: string | null;
  revisionId: string | null;
  shareId: string | null;
  totalBytes: number;
  vaultId: string;
}

export type ObjectContentKind = "manifest" | "chunk";

export interface ObjectRevisionCryptoContext {
  contentKind: ObjectContentKind;
  index: number;
  objectId: string;
  revisionId: string;
  sourceFormat: string;
  total: number;
  vaultId: string;
}

function lengthPrefixed(parts: readonly Uint8Array[]): Uint8Array {
  const encoded: Uint8Array[] = [];
  for (const part of parts) {
    encoded.push(u32(part.length), part);
  }
  return concatBytes(encoded);
}

function field(value: number | string | Uint8Array): Uint8Array {
  if (value instanceof Uint8Array) return value;
  return utf8(String(value));
}

export function record(
  ...fields: readonly (number | string | Uint8Array)[]
): Uint8Array {
  return lengthPrefixed(fields.map(field));
}

export function canonicalRequestBytes(
  input: CanonicalRequestInput,
): Uint8Array {
  const sortedQuery = [...input.query].sort(
    ([leftKey, leftValue], [rightKey, rightValue]) => {
      if (leftKey !== rightKey) return leftKey < rightKey ? -1 : 1;
      if (leftValue !== rightValue) return leftValue < rightValue ? -1 : 1;
      return 0;
    },
  );
  const queryBytes = lengthPrefixed(
    sortedQuery.map(([key, value]) => record(key, value)),
  );
  return record(
    "sona-request-v1",
    input.audience,
    input.vaultId,
    input.deviceId,
    input.method,
    input.path,
    queryBytes,
    input.bodyDigest,
    input.contentType,
    input.idempotencyKey,
    input.timestamp,
    input.nonce,
  );
}

export function canonicalBootstrapBytes(input: {
  audience: string;
  deviceId: string;
  pairingPublicKey: Uint8Array;
  signingPublicKey: Uint8Array;
  vaultId: string;
}): Uint8Array {
  return record(
    "sona-bootstrap-v1",
    input.audience,
    input.vaultId,
    input.deviceId,
    input.signingPublicKey,
    input.pairingPublicKey,
  );
}

export function canonicalPairCandidateBytes(input: {
  audience: string;
  candidateDeviceId: string;
  candidatePairingPublicKey: Uint8Array;
  candidateSigningPublicKey: Uint8Array;
  expiresAt: number;
  pairingNonce: Uint8Array;
  vaultId: string;
}): Uint8Array {
  return record(
    "sona-pair-candidate-v1",
    input.audience,
    input.vaultId,
    input.candidateDeviceId,
    input.candidateSigningPublicKey,
    input.candidatePairingPublicKey,
    input.pairingNonce,
    input.expiresAt,
  );
}

export function canonicalPairApprovalBytes(input: {
  candidateProof: Uint8Array;
  candidateRecord: Uint8Array;
  envelope: Uint8Array;
  vaultId: string;
}): Uint8Array {
  return record(
    "sona-pair-approval-v1",
    input.vaultId,
    input.candidateRecord,
    input.candidateProof,
    input.envelope,
  );
}

export function canonicalUploadEnvelopeBytes(
  input: CanonicalUploadEnvelopeInput,
): Uint8Array {
  const chunkBytes = lengthPrefixed(
    input.chunks.map((chunk) => record(chunk.index, chunk.size, chunk.sha256)),
  );
  return record(
    "sona-upload-envelope-v1",
    input.vaultId,
    input.kind,
    input.objectId ?? "",
    input.revisionId ?? "",
    input.baseRevisionId ?? "",
    input.shareId ?? "",
    input.manifestDigest,
    input.cryptoVersion,
    input.totalBytes,
    input.chunks.length,
    chunkBytes,
  );
}

export function canonicalTombstoneBytes(input: {
  baseRevisionId: string;
  formatVersion: number;
  objectId: string;
  reason: string;
  tombstoneRevisionId: string;
  vaultId: string;
}): Uint8Array {
  return record(
    "sona-tombstone-v1",
    input.vaultId,
    input.objectId,
    input.tombstoneRevisionId,
    input.baseRevisionId,
    input.reason,
    input.formatVersion,
  );
}

export async function verifyEd25519(
  publicKey: Uint8Array,
  signature: Uint8Array,
  message: Uint8Array,
): Promise<boolean> {
  if (publicKey.length !== 32 || signature.length !== 64) return false;
  const key = await crypto.subtle.importKey(
    "raw",
    publicKey,
    { name: "Ed25519" },
    false,
    ["verify"],
  );
  return crypto.subtle.verify({ name: "Ed25519" }, key, signature, message);
}

function assertObjectRevisionContext(input: ObjectRevisionCryptoContext): void {
  if (
    !Number.isSafeInteger(input.index) ||
    !Number.isSafeInteger(input.total) ||
    input.index < 0 ||
    input.total < 1 ||
    input.index >= input.total ||
    input.sourceFormat.length === 0
  ) {
    throw new Error("invalid object revision context");
  }
}

function objectRevisionAad(input: ObjectRevisionCryptoContext): Uint8Array {
  return record(
    "sona-object-aad-v1",
    input.vaultId,
    input.objectId,
    input.revisionId,
    input.index,
    input.total,
    input.contentKind,
    input.sourceFormat,
  );
}

export async function deriveAesGcmKey(
  material: Uint8Array,
  salt: Uint8Array,
  info: Uint8Array,
): Promise<CryptoKey> {
  const inputKey = await crypto.subtle.importKey(
    "raw",
    material,
    "HKDF",
    false,
    ["deriveKey"],
  );
  return crypto.subtle.deriveKey(
    { name: "HKDF", hash: "SHA-256", salt, info },
    inputKey,
    { name: "AES-GCM", length: 256 },
    false,
    ["decrypt", "encrypt"],
  );
}

export async function deriveObjectRevisionRoot(input: {
  objectId: string;
  revisionId: string;
  vaultId: string;
  vaultRoot: Uint8Array;
}): Promise<Uint8Array> {
  if (input.vaultRoot.length !== 32)
    throw new Error("invalid object revision root material");
  const key = await crypto.subtle.importKey(
    "raw",
    input.vaultRoot,
    "HKDF",
    false,
    ["deriveBits"],
  );
  const bits = await crypto.subtle.deriveBits(
    {
      name: "HKDF",
      hash: "SHA-256",
      salt: utf8("sona-revision-v1"),
      info: record(
        "sona-revision-root-v1",
        input.vaultId,
        input.objectId,
        input.revisionId,
      ),
    },
    key,
    256,
  );
  return new Uint8Array(bits);
}

async function objectRevisionKey(
  input: ObjectRevisionCryptoContext & { vaultRoot: Uint8Array },
): Promise<CryptoKey> {
  assertObjectRevisionContext(input);
  const revisionRoot = await deriveObjectRevisionRoot(input);
  return deriveAesGcmKey(
    revisionRoot,
    utf8("sona-object-v1"),
    record(
      "sona-object-key-v1",
      input.vaultId,
      input.objectId,
      input.revisionId,
      input.index,
      input.total,
      input.contentKind,
      input.sourceFormat,
    ),
  );
}

export async function decryptObjectRevisionPayload(
  input: ObjectRevisionCryptoContext & {
    ciphertext: Uint8Array;
    vaultRoot: Uint8Array;
  },
): Promise<Uint8Array> {
  if (input.ciphertext.length < 28)
    throw new Error("invalid encrypted object revision payload");
  const key = await objectRevisionKey(input);
  const decrypted = await crypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: input.ciphertext.subarray(0, 12),
      additionalData: objectRevisionAad(input),
      tagLength: 128,
    },
    key,
    input.ciphertext.subarray(12),
  );
  return new Uint8Array(decrypted);
}

export async function encryptObjectRevisionPayload(
  input: ObjectRevisionCryptoContext & {
    nonce: Uint8Array;
    plaintext: Uint8Array;
    vaultRoot: Uint8Array;
  },
): Promise<Uint8Array> {
  if (input.nonce.length !== 12)
    throw new Error("invalid object revision encryption material");
  const key = await objectRevisionKey(input);
  const encrypted = await crypto.subtle.encrypt(
    {
      name: "AES-GCM",
      iv: input.nonce,
      additionalData: objectRevisionAad(input),
      tagLength: 128,
    },
    key,
    input.plaintext,
  );
  return concatBytes([input.nonce, new Uint8Array(encrypted)]);
}

function shareAad(
  shareId: string,
  index: number,
  total: number,
  domain: "manifest" | "chunk",
): Uint8Array {
  return record("sona-share-aad-v1", shareId, index, total, domain);
}

async function shareKey(
  root: Uint8Array,
  shareId: string,
  index: number,
  total: number,
  domain: "manifest" | "chunk",
): Promise<CryptoKey> {
  return deriveAesGcmKey(
    root,
    utf8("sona-share-v1"),
    record("sona-share-key-v1", shareId, index, total, domain),
  );
}

export async function decryptSharePayload(input: {
  ciphertext: Uint8Array;
  domain: "manifest" | "chunk";
  index: number;
  root: Uint8Array;
  shareId: string;
  total: number;
}): Promise<Uint8Array> {
  if (input.root.length !== 32 || input.ciphertext.length < 28) {
    throw new Error("invalid encrypted share payload");
  }
  const nonce = input.ciphertext.subarray(0, 12);
  const encrypted = input.ciphertext.subarray(12);
  const key = await shareKey(
    input.root,
    input.shareId,
    input.index,
    input.total,
    input.domain,
  );
  const decrypted = await crypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: nonce,
      additionalData: shareAad(
        input.shareId,
        input.index,
        input.total,
        input.domain,
      ),
      tagLength: 128,
    },
    key,
    encrypted,
  );
  return new Uint8Array(decrypted);
}

export async function encryptSharePayload(input: {
  domain: "manifest" | "chunk";
  index: number;
  nonce: Uint8Array;
  plaintext: Uint8Array;
  root: Uint8Array;
  shareId: string;
  total: number;
}): Promise<Uint8Array> {
  if (input.root.length !== 32 || input.nonce.length !== 12) {
    throw new Error("invalid share encryption material");
  }
  const key = await shareKey(
    input.root,
    input.shareId,
    input.index,
    input.total,
    input.domain,
  );
  const encrypted = await crypto.subtle.encrypt(
    {
      name: "AES-GCM",
      iv: input.nonce,
      additionalData: shareAad(
        input.shareId,
        input.index,
        input.total,
        input.domain,
      ),
      tagLength: 128,
    },
    key,
    input.plaintext,
  );
  return concatBytes([input.nonce, new Uint8Array(encrypted)]);
}
