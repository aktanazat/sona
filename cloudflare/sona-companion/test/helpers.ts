import { SELF } from "cloudflare:test";

import {
  canonicalBootstrapBytes,
  canonicalRequestBytes,
  canonicalTombstoneBytes,
  canonicalUploadEnvelopeBytes,
} from "../src/crypto";
import {
  base64UrlEncode,
  randomId,
  sha256Base64Url,
  utf8,
} from "../src/encoding";
import { isJsonRecord, type JsonRecord } from "../src/json";

export interface Device {
  deviceId: string;
  pairingPublicKey: Uint8Array;
  signingPrivateKey: CryptoKey;
  signingPublicKey: Uint8Array;
  vaultId: string;
}

export interface UploadPayloadJson {
  baseRevisionId?: string | null;
  chunkCount: number;
  chunks: { index: number; sha256: string; size: number }[];
  cryptoVersion: number;
  expiresAt?: number;
  manifest: string;
  manifestSha256: string;
  objectId?: string | null;
  revisionId?: string | null;
  shareId?: string | undefined;
  totalBytes: number;
  uploadId: string;
  version: number;
  writerSignature: string;
}

export interface UploadPayload {
  chunks: Uint8Array[];
  json: UploadPayloadJson;
}

function isKeyPair(value: CryptoKey | CryptoKeyPair): value is CryptoKeyPair {
  return "privateKey" in value;
}

export function testId(prefix: string): string {
  return `${prefix}_${randomId(12)}`;
}

export async function makeDevice(vaultId = testId("vault")): Promise<Device> {
  const generated = await crypto.subtle.generateKey({ name: "Ed25519" }, true, [
    "sign",
    "verify",
  ]);
  if (!isKeyPair(generated))
    throw new Error("Ed25519 did not return a key pair");
  const signingPublicKey = new Uint8Array(
    await crypto.subtle.exportKey("raw", generated.publicKey),
  );
  const pairingPublicKey = new Uint8Array(32);
  crypto.getRandomValues(pairingPublicKey);
  return {
    vaultId,
    deviceId: testId("device"),
    signingPrivateKey: generated.privateKey,
    signingPublicKey,
    pairingPublicKey,
  };
}

export async function bootstrap(
  device: Device,
  idempotencyKey = testId("bootstrap"),
): Promise<Response> {
  const signature = new Uint8Array(
    await crypto.subtle.sign(
      { name: "Ed25519" },
      device.signingPrivateKey,
      canonicalBootstrapBytes({
        audience: "sona-companion",
        vaultId: device.vaultId,
        deviceId: device.deviceId,
        signingPublicKey: device.signingPublicKey,
        pairingPublicKey: device.pairingPublicKey,
      }),
    ),
  );
  const body = JSON.stringify({
    version: 1,
    vaultId: device.vaultId,
    deviceId: device.deviceId,
    signingPublicKey: base64UrlEncode(device.signingPublicKey),
    pairingPublicKey: base64UrlEncode(device.pairingPublicKey),
    selfSignature: base64UrlEncode(signature),
  });
  return SELF.fetch("https://companion.test/v1/bootstrap/device", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "content-length": String(utf8(body).length),
      "x-sona-bootstrap-secret": "test-bootstrap-secret-only",
      "x-sona-idempotency-key": idempotencyKey,
    },
    body,
  });
}

export async function signedRequest(
  device: Device,
  input: {
    body?: string | Uint8Array;
    contentType?: string;
    idempotencyKey?: string;
    method: string;
    path: string;
    timestamp?: number;
  },
): Promise<Request> {
  const body =
    input.body === undefined
      ? new Uint8Array()
      : input.body instanceof Uint8Array
        ? input.body
        : utf8(input.body);
  const url = new URL(input.path, "https://companion.test");
  const query: [string, string][] = [];
  url.searchParams.forEach((value, key) => query.push([key, value]));
  const nonce = new Uint8Array(16);
  crypto.getRandomValues(nonce);
  const timestamp = input.timestamp ?? Date.now();
  const contentType =
    input.contentType ?? (input.body === undefined ? "" : "application/json");
  const idempotencyKey = input.idempotencyKey ?? "";
  const bodyDigest = await sha256Base64Url(body);
  const signature = new Uint8Array(
    await crypto.subtle.sign(
      { name: "Ed25519" },
      device.signingPrivateKey,
      canonicalRequestBytes({
        audience: "sona-companion",
        vaultId: device.vaultId,
        deviceId: device.deviceId,
        method: input.method,
        path: url.pathname,
        query,
        bodyDigest,
        contentType,
        idempotencyKey,
        timestamp,
        nonce,
      }),
    ),
  );
  const headers = new Headers({
    "x-sona-vault-id": device.vaultId,
    "x-sona-device-id": device.deviceId,
    "x-sona-timestamp": String(timestamp),
    "x-sona-nonce": base64UrlEncode(nonce),
    "x-sona-signature": base64UrlEncode(signature),
  });
  if (contentType.length > 0) headers.set("content-type", contentType);
  if (idempotencyKey.length > 0)
    headers.set("x-sona-idempotency-key", idempotencyKey);
  if (input.body !== undefined)
    headers.set("content-length", String(body.length));
  const init: RequestInit = {
    method: input.method,
    headers,
  };
  if (input.body !== undefined) init.body = body;
  return new Request(url, init);
}

export async function signedFetch(
  device: Device,
  input: {
    body?: string | Uint8Array;
    contentType?: string;
    idempotencyKey?: string;
    method: string;
    path: string;
    timestamp?: number;
  },
): Promise<Response> {
  return SELF.fetch(await signedRequest(device, input));
}

export async function createUploadPayload(
  device: Device,
  input: {
    baseRevisionId?: string | null;
    chunks?: Uint8Array[];
    manifest?: Uint8Array;
    objectId?: string;
    revisionId?: string;
    shareExpiresAt?: number;
    shareId?: string;
    uploadId?: string;
  } = {},
): Promise<UploadPayload> {
  const chunks = input.chunks ?? [new Uint8Array(28)];
  const manifest = input.manifest ?? new Uint8Array(28);
  const objectMode = input.shareId === undefined;
  const uploadId = input.uploadId ?? testId("upload");
  const objectId = input.objectId ?? (objectMode ? testId("object") : null);
  const revisionId =
    input.revisionId ?? (objectMode ? testId("revision") : null);
  const shareId = input.shareId;
  const plannedChunks = await Promise.all(
    chunks.map(async (chunk, index) => ({
      index,
      size: chunk.length,
      sha256: await sha256Base64Url(chunk),
    })),
  );
  const totalBytes = plannedChunks.reduce(
    (total, chunk) => total + chunk.size,
    0,
  );
  const writerSignature = new Uint8Array(
    await crypto.subtle.sign(
      { name: "Ed25519" },
      device.signingPrivateKey,
      canonicalUploadEnvelopeBytes({
        vaultId: device.vaultId,
        kind: objectMode ? "object" : "share",
        objectId,
        revisionId,
        baseRevisionId: input.baseRevisionId ?? null,
        shareId: shareId ?? null,
        manifestDigest: await sha256Base64Url(manifest),
        cryptoVersion: 1,
        totalBytes,
        chunks: plannedChunks,
      }),
    ),
  );
  const json: UploadPayloadJson = {
    version: 1,
    uploadId,
    manifest: base64UrlEncode(manifest),
    manifestSha256: await sha256Base64Url(manifest),
    chunkCount: plannedChunks.length,
    chunks: plannedChunks,
    totalBytes,
    cryptoVersion: 1,
    writerSignature: base64UrlEncode(writerSignature),
  };
  if (objectMode) {
    json.objectId = objectId;
    json.revisionId = revisionId;
    json.baseRevisionId = input.baseRevisionId ?? null;
  } else {
    json.shareId = shareId;
    json.expiresAt = input.shareExpiresAt ?? Date.now() + 60_000;
  }
  return { json, chunks };
}

export async function uploadAllChunks(
  device: Device,
  uploadId: string,
  chunks: readonly Uint8Array[],
): Promise<void> {
  for (let index = 0; index < chunks.length; index += 1) {
    const chunk = chunks[index];
    if (chunk === undefined) throw new Error("missing test chunk");
    const request = await signedRequest(device, {
      method: "PUT",
      path: `/v1/uploads/${uploadId}/chunks/${index}`,
      contentType: "application/octet-stream",
      body: chunk,
      idempotencyKey: testId("chunk_operation"),
    });
    request.headers.set("x-sona-chunk-sha256", await sha256Base64Url(chunk));
    const response = await SELF.fetch(request);
    if (!response.ok)
      throw new Error(`chunk upload failed with ${response.status}`);
  }
}

export async function jsonBody(response: Response): Promise<JsonRecord> {
  const body: unknown = await response.json();
  if (!isJsonRecord(body)) {
    throw new Error("expected an object response");
  }
  return body;
}

export async function tombstonePayload(
  device: Device,
  objectId: string,
  baseRevisionId: string,
): Promise<string> {
  const tombstoneRevisionId = testId("tombstone");
  const writerSignature = new Uint8Array(
    await crypto.subtle.sign(
      { name: "Ed25519" },
      device.signingPrivateKey,
      canonicalTombstoneBytes({
        vaultId: device.vaultId,
        objectId,
        tombstoneRevisionId,
        baseRevisionId,
        reason: "user_request",
        formatVersion: 1,
      }),
    ),
  );
  return JSON.stringify({
    tombstoneRevisionId,
    baseRevisionId,
    formatVersion: 1,
    reason: "user_request",
    writerSignature: base64UrlEncode(writerSignature),
  });
}
