import {
  CRYPTO_VERSION,
  LIVE_ROOM_GONE,
  LIVE_ROOM_TTL_MS,
  MAX_ACTIVE_SHARES,
  MAX_ACTIVE_UPLOADS,
  MAX_CHANGE_PAGE,
  MAX_CHUNK_BYTES,
  MAX_CHUNKS_PER_UPLOAD,
  MAX_CLOCK_SKEW_MS,
  MAX_MANIFEST_BYTES,
  MAX_JSON_BYTES,
  MAX_REMOTE_BYTES,
  MAX_SHARE_BYTES,
  MAX_SHARE_TTL_MS,
  MAX_SNAPSHOT_PAGE,
  NONCE_RETENTION_MS,
  PROTOCOL_VERSION,
  UPLOAD_TTL_MS,
  type TombstoneReason,
} from "./constants";
import {
  canonicalBootstrapBytes,
  canonicalPairApprovalBytes,
  canonicalPairCandidateBytes,
  canonicalRequestBytes,
  canonicalTombstoneBytes,
  canonicalUploadEnvelopeBytes,
  verifyEd25519,
} from "./crypto";
import {
  activeDevice,
  batchResult,
  bytesFromDb,
  changeCount,
  claimMutationNonce,
  deviceKeyText,
  idempotencyResult,
  newMaintenanceJob,
  objectHead,
  planFromJson,
  planToJson,
  r2ChunkKey,
  responseDigestHeader,
  scheduleMaintenance,
  shareRow,
  uploadSession,
} from "./db";
import {
  base64UrlDecode,
  base64UrlEncode,
  decodeUtf8,
  equalBytes,
  equalSecret,
  isIdempotencyKey,
  isOpaqueId,
  randomId,
  sha256Base64Url,
  utf8,
} from "./encoding";
import {
  ApiProblem,
  bytesResponse,
  dependencyProblem,
  errorResponse,
  jsonResponse,
  logRequest,
  problem,
} from "./errors";
import { LIVE_VERIFIER_HEADER } from "./live";
import { maintenanceQueueMessage, runMaintenance } from "./maintenance";
import type {
  AuthenticatedRequest,
  Env,
  IdempotencyRow,
  RequestContext,
  UploadPlan,
  UploadPlanChunk,
  UploadSessionRow,
} from "./types";
import {
  asRecord,
  assertExactKeys,
  boundedBase64Url,
  canonicalQuery,
  digest,
  fixedBase64Url,
  isJsonArray,
  isJsonInteger,
  isJsonRecord,
  isJsonString,
  optionalOpaqueId,
  parseJsonBody,
  readLimitedBody,
  requireContentType,
  requiredInteger,
  requiredOpaqueId,
  requiredString,
  routeId,
  type JsonValue,
} from "./validation";

const emptyDigest = "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU";
const noBody = new Uint8Array();
const minimumEncryptedPayloadBytes = 12 + 16;
const MAX_CONCURRENT_R2_READS = 8;
const MAX_CONCURRENT_QUEUE_JOBS = 8;

type RouteName =
  | "assets"
  | "bootstrap"
  | "capabilities"
  | "changes"
  | "device_delete"
  | "devices"
  | "devices_pair"
  | "devices_self"
  | "health"
  | "live_create"
  | "live_page"
  | "live_socket"
  | "object_chunk"
  | "object_delete"
  | "object_manifest"
  | "share_chunk"
  | "share_delete"
  | "share_download"
  | "share_manifest"
  | "share_page"
  | "shares"
  | "snapshot"
  | "upload_cancel"
  | "upload_chunk"
  | "upload_commit"
  | "upload_create"
  | "upload_status";

interface Route {
  ids: Record<string, string>;
  name: RouteName;
  queryKeys: readonly string[];
}

interface StoredResponse {
  body: unknown;
  status: number;
}

interface ParsedPairing {
  approvalSignature: Uint8Array;
  candidateDeviceId: string;
  candidatePairingPublicKey: Uint8Array;
  candidateProof: Uint8Array;
  candidateSigningPublicKey: Uint8Array;
  envelope: Uint8Array;
  expiresAt: number;
  pairingNonce: Uint8Array;
}

interface ParsedTombstone {
  baseRevisionId: string;
  formatVersion: number;
  reason: TombstoneReason;
  tombstoneRevisionId: string;
  writerSignature: Uint8Array;
}

function capabilities() {
  return {
    protocol_version: PROTOCOL_VERSION,
    crypto_version: CRYPTO_VERSION,
    request_auth: {
      algorithm: "Ed25519",
      clock_skew_seconds: MAX_CLOCK_SKEW_MS / 1000,
      nonce_retention_seconds: NONCE_RETENTION_MS / 1000,
    },
    limits: {
      remote_bytes: MAX_REMOTE_BYTES,
      chunk_bytes: MAX_CHUNK_BYTES,
      chunks_per_upload: MAX_CHUNKS_PER_UPLOAD,
      active_uploads: MAX_ACTIVE_UPLOADS,
      active_shares: MAX_ACTIVE_SHARES,
      share_bytes: MAX_SHARE_BYTES,
      share_expiry_seconds: MAX_SHARE_TTL_MS / 1000,
      change_page: MAX_CHANGE_PAGE,
      snapshot_page: MAX_SNAPSHOT_PAGE,
    },
  };
}

function routeFor(pathname: string): Route {
  if (pathname.includes("%") || pathname.includes("//"))
    throw problem("invalid_request");
  if (pathname === "/healthz")
    return { name: "health", ids: {}, queryKeys: [] };
  if (pathname === "/v1/capabilities")
    return { name: "capabilities", ids: {}, queryKeys: [] };
  if (pathname === "/v1/bootstrap/device")
    return { name: "bootstrap", ids: {}, queryKeys: [] };
  if (pathname === "/v1/devices")
    return { name: "devices", ids: {}, queryKeys: [] };
  if (pathname === "/v1/devices/self")
    return { name: "devices_self", ids: {}, queryKeys: [] };
  if (pathname === "/v1/devices/pair")
    return { name: "devices_pair", ids: {}, queryKeys: [] };
  if (pathname === "/v1/uploads")
    return { name: "upload_create", ids: {}, queryKeys: [] };
  if (pathname === "/v1/shares")
    return { name: "shares", ids: {}, queryKeys: [] };
  if (pathname === "/v1/changes")
    return { name: "changes", ids: {}, queryKeys: ["cursor", "limit"] };
  if (pathname === "/v1/snapshot")
    return {
      name: "snapshot",
      ids: {},
      queryKeys: ["after", "highWater", "limit"],
    };
  if (pathname === "/v1/live")
    return { name: "live_create", ids: {}, queryKeys: [] };

  const segments = pathname.split("/");
  if (segments.length === 3 && segments[1] === "s") {
    return {
      name: "share_page",
      ids: { shareId: routeId(segments[2]) },
      queryKeys: [],
    };
  }
  if (segments.length === 3 && segments[1] === "l") {
    return {
      name: "live_page",
      ids: { roomId: routeId(segments[2]) },
      queryKeys: [],
    };
  }
  if (
    segments.length === 5 &&
    segments[1] === "v1" &&
    segments[2] === "live" &&
    segments[4] === "socket"
  ) {
    return {
      name: "live_socket",
      ids: { roomId: routeId(segments[3]) },
      queryKeys: ["t"],
    };
  }
  if (
    segments.length === 4 &&
    segments[1] === "v1" &&
    segments[2] === "devices"
  ) {
    return {
      name: "device_delete",
      ids: { deviceId: routeId(segments[3]) },
      queryKeys: [],
    };
  }
  if (
    segments.length === 4 &&
    segments[1] === "v1" &&
    segments[2] === "uploads"
  ) {
    return {
      name: "upload_status",
      ids: { uploadId: routeId(segments[3]) },
      queryKeys: [],
    };
  }
  if (
    segments.length === 5 &&
    segments[1] === "v1" &&
    segments[2] === "uploads" &&
    segments[4] === "commit"
  ) {
    return {
      name: "upload_commit",
      ids: { uploadId: routeId(segments[3]) },
      queryKeys: [],
    };
  }
  if (
    segments.length === 6 &&
    segments[1] === "v1" &&
    segments[2] === "uploads" &&
    segments[4] === "chunks"
  ) {
    return {
      name: "upload_chunk",
      ids: {
        uploadId: routeId(segments[3]),
        index: requiredIndex(segments[5]),
      },
      queryKeys: [],
    };
  }
  if (
    segments.length === 5 &&
    segments[1] === "v1" &&
    segments[2] === "shares"
  ) {
    const shareId = routeId(segments[3]);
    if (segments[4] === "manifest")
      return { name: "share_manifest", ids: { shareId }, queryKeys: [] };
    if (segments[4] === "file")
      return { name: "share_download", ids: { shareId }, queryKeys: [] };
  }
  if (
    segments.length === 6 &&
    segments[1] === "v1" &&
    segments[2] === "shares" &&
    segments[4] === "chunks"
  ) {
    return {
      name: "share_chunk",
      ids: { shareId: routeId(segments[3]), index: requiredIndex(segments[5]) },
      queryKeys: [],
    };
  }
  if (
    segments.length === 4 &&
    segments[1] === "v1" &&
    segments[2] === "shares"
  ) {
    return {
      name: "share_delete",
      ids: { shareId: routeId(segments[3]) },
      queryKeys: [],
    };
  }
  if (
    segments.length === 7 &&
    segments[1] === "v1" &&
    segments[2] === "objects" &&
    segments[4] === "revisions" &&
    segments[6] === "manifest"
  ) {
    return {
      name: "object_manifest",
      ids: { objectId: routeId(segments[3]), revisionId: routeId(segments[5]) },
      queryKeys: [],
    };
  }
  if (
    segments.length === 8 &&
    segments[1] === "v1" &&
    segments[2] === "objects" &&
    segments[4] === "revisions" &&
    segments[6] === "chunks"
  ) {
    return {
      name: "object_chunk",
      ids: {
        objectId: routeId(segments[3]),
        revisionId: routeId(segments[5]),
        index: requiredIndex(segments[7]),
      },
      queryKeys: [],
    };
  }
  if (
    segments.length === 4 &&
    segments[1] === "v1" &&
    segments[2] === "objects"
  ) {
    return {
      name: "object_delete",
      ids: { objectId: routeId(segments[3]) },
      queryKeys: [],
    };
  }
  if (pathname.startsWith("/v1/")) throw problem("not_found");
  return { name: "assets", ids: {}, queryKeys: [] };
}

function requiredIndex(value: string | undefined): string {
  if (
    value === undefined ||
    !/^(0|[1-9]\d{0,3})$/u.test(value) ||
    Number(value) >= MAX_CHUNKS_PER_UPLOAD
  ) {
    throw problem("invalid_request");
  }
  return value;
}

function methodIs(request: Request, expected: string): void {
  if (request.method !== expected) throw problem("not_found");
}

function parseLimit(
  value: string | null,
  defaultValue: number,
  maximum: number,
): number {
  if (value === null) return defaultValue;
  if (!/^[1-9]\d{0,2}$/u.test(value)) throw problem("invalid_request");
  const limit = Number(value);
  if (limit > maximum) throw problem("invalid_request");
  return limit;
}

function encodeChangeCursor(sequence: number): string {
  return `c.${base64UrlEncode(utf8(JSON.stringify({ v: 1, a: sequence })))}`;
}

function decodeChangeCursor(value: string | null): number {
  if (value === null) return 0;
  if (!value.startsWith("c.")) throw problem("invalid_request");
  const bytes = base64UrlDecode(value.slice(2));
  if (bytes === null) throw problem("invalid_request");
  try {
    const record = asRecord(JSON.parse(decodeUtf8(bytes)));
    const after = record.a;
    if (record.v !== 1 || !isJsonInteger(after) || after < 0) {
      throw problem("invalid_request");
    }
    return after;
  } catch (error) {
    if (error instanceof ApiProblem) throw error;
    throw problem("invalid_request");
  }
}

function encodeSnapshotHighWater(highWater: number): string {
  return `h.${base64UrlEncode(utf8(JSON.stringify({ v: 1, w: highWater })))}`;
}

function decodeSnapshotHighWater(value: string): number {
  if (!value.startsWith("h.")) throw problem("invalid_request");
  const bytes = base64UrlDecode(value.slice(2));
  if (bytes === null) throw problem("invalid_request");
  try {
    const record = asRecord(JSON.parse(decodeUtf8(bytes)));
    const highWater = record.w;
    if (record.v !== 1 || !isJsonInteger(highWater) || highWater < 0) {
      throw problem("invalid_request");
    }
    return highWater;
  } catch (error) {
    if (error instanceof ApiProblem) throw error;
    throw problem("invalid_request");
  }
}

function encodeSnapshotAfter(highWater: number, after: string): string {
  return `p.${base64UrlEncode(utf8(JSON.stringify({ v: 1, w: highWater, a: after })))}`;
}

function decodeSnapshotAfter(value: string | null, highWater: number): string {
  if (value === null) return "";
  if (!value.startsWith("p.")) throw problem("invalid_request");
  const bytes = base64UrlDecode(value.slice(2));
  if (bytes === null) throw problem("invalid_request");
  try {
    const record = asRecord(JSON.parse(decodeUtf8(bytes)));
    const after = record.a;
    if (
      record.v !== 1 ||
      record.w !== highWater ||
      !isJsonString(after) ||
      !isOpaqueId(after)
    ) {
      throw problem("invalid_request");
    }
    return after;
  } catch (error) {
    if (error instanceof ApiProblem) throw error;
    throw problem("invalid_request");
  }
}

async function parseStoredResponse(
  row: IdempotencyRow,
): Promise<StoredResponse> {
  try {
    return { body: JSON.parse(row.response_json), status: row.status };
  } catch {
    throw problem("integrity_failed");
  }
}

async function existingMutationResponse(
  env: Env,
  auth: AuthenticatedRequest,
  operation: string,
  target: string,
): Promise<Response | null> {
  const key = auth.idempotencyKey;
  if (key === undefined) throw problem("invalid_request");
  const existing = await idempotencyResult(
    env,
    auth.vaultId,
    auth.deviceId,
    key,
  );
  if (existing === null) return null;
  if (
    existing.operation !== operation ||
    existing.target !== target ||
    existing.body_digest !== auth.bodyDigest
  ) {
    throw problem("idempotency_conflict");
  }
  const stored = await parseStoredResponse(existing);
  return jsonResponse(stored.body, stored.status);
}

async function authenticateUnclaimed(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  mutation: boolean,
  now: number,
): Promise<AuthenticatedRequest> {
  const vaultId = request.headers.get("x-sona-vault-id") ?? "";
  const deviceId = request.headers.get("x-sona-device-id") ?? "";
  if (!isOpaqueId(vaultId) || !isOpaqueId(deviceId))
    throw problem("unauthorized");
  const timestampText = request.headers.get("x-sona-timestamp") ?? "";
  if (!/^\d{13}$/u.test(timestampText)) throw problem("invalid_request");
  const timestamp = Number(timestampText);
  if (Math.abs(now - timestamp) > MAX_CLOCK_SKEW_MS)
    throw problem("clock_skew");
  const nonce = fixedBase64Url(request.headers.get("x-sona-nonce") ?? "", 16);
  const signature = fixedBase64Url(
    request.headers.get("x-sona-signature") ?? "",
    64,
  );
  const idempotencyKey = request.headers.get("x-sona-idempotency-key") ?? "";
  if (mutation && !isIdempotencyKey(idempotencyKey))
    throw problem("invalid_request");
  if (!mutation && idempotencyKey.length > 0) throw problem("invalid_request");
  const query = canonicalQuery(new URL(request.url), route.queryKeys);
  const contentType = request.headers.get("content-type") ?? "";
  const [bodyDigest, device] = await Promise.all([
    sha256Base64Url(body),
    activeDevice(env, vaultId, deviceId),
  ]);
  if (device === null) throw problem("unauthorized");
  if (device.status === "revoked") throw problem("revoked_device");
  const signingPublicKey = bytesFromDb(device.signing_public_key);
  const verified = await verifyEd25519(
    signingPublicKey,
    signature,
    canonicalRequestBytes({
      audience: env.AUDIENCE,
      vaultId,
      deviceId,
      method: request.method,
      path: new URL(request.url).pathname,
      query,
      bodyDigest,
      contentType,
      idempotencyKey,
      timestamp,
      nonce,
    }),
  );
  if (!verified) throw problem("unauthorized");
  const authenticated: AuthenticatedRequest = {
    vaultId,
    deviceId,
    timestamp,
    nonce,
    signature,
    signingPublicKey,
    body,
    bodyDigest,
  };
  if (mutation) authenticated.idempotencyKey = idempotencyKey;
  return authenticated;
}

async function authenticate(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  mutation: boolean,
  now: number,
): Promise<AuthenticatedRequest> {
  const authenticated = await authenticateUnclaimed(
    request,
    env,
    route,
    body,
    mutation,
    now,
  );
  if (mutation) {
    await claimMutationNonce(
      env,
      authenticated.vaultId,
      authenticated.deviceId,
      authenticated.nonce,
      now,
      now + NONCE_RETENTION_MS,
    );
  }
  return authenticated;
}

async function forEachBounded<Value>(
  values: readonly Value[],
  limit: number,
  operation: (value: Value) => Promise<void>,
): Promise<void> {
  const iterator = values.values();
  const worker = async (): Promise<void> => {
    const next = iterator.next();
    if (next.done) return;
    await operation(next.value);
    return worker();
  };
  const settled = await Promise.allSettled(
    Array.from({ length: Math.min(limit, values.length) }, () => worker()),
  );
  for (const result of settled) {
    if (result.status === "rejected") throw result.reason;
  }
}

function parseChunks(
  value: JsonValue | undefined,
  chunkCount: number,
): UploadPlanChunk[] {
  if (!isJsonArray(value) || value.length !== chunkCount)
    throw problem("invalid_request");
  const chunks: UploadPlanChunk[] = [];
  for (let index = 0; index < value.length; index += 1) {
    const chunk = asRecord(value[index]);
    assertExactKeys(chunk, ["index", "sha256", "size"]);
    const chunkIndex = requiredInteger(
      chunk,
      "index",
      0,
      MAX_CHUNKS_PER_UPLOAD - 1,
    );
    const size = requiredInteger(
      chunk,
      "size",
      minimumEncryptedPayloadBytes,
      MAX_CHUNK_BYTES,
    );
    const sha256 = digest(requiredString(chunk, "sha256", 64));
    if (chunkIndex !== index) throw problem("invalid_request");
    if (
      chunkCount > 1 &&
      chunkIndex < chunkCount - 1 &&
      size !== MAX_CHUNK_BYTES
    ) {
      throw problem("invalid_request");
    }
    chunks.push({ index: chunkIndex, size, sha256 });
  }
  return chunks;
}

async function parseUploadPlan(
  body: Uint8Array,
  kind: "object" | "share",
  now: number,
): Promise<UploadPlan> {
  const value = parseJsonBody(body);
  const commonKeys = [
    "chunkCount",
    "chunks",
    "cryptoVersion",
    "manifest",
    "manifestSha256",
    "totalBytes",
    "uploadId",
    "version",
    "writerSignature",
  ];
  const objectKeys = [
    ...commonKeys,
    "baseRevisionId",
    "objectId",
    "revisionId",
  ];
  const shareKeys = [...commonKeys, "expiresAt", "shareId"];
  assertExactKeys(value, kind === "object" ? objectKeys : shareKeys);
  if (
    requiredInteger(value, "version", PROTOCOL_VERSION, PROTOCOL_VERSION) !==
    PROTOCOL_VERSION
  ) {
    throw problem("unsupported_version");
  }
  const cryptoVersion = requiredInteger(
    value,
    "cryptoVersion",
    CRYPTO_VERSION,
    CRYPTO_VERSION,
  );
  if (cryptoVersion !== CRYPTO_VERSION) throw problem("unsupported_version");
  const uploadId = requiredOpaqueId(value, "uploadId");
  const chunkCount = requiredInteger(
    value,
    "chunkCount",
    0,
    MAX_CHUNKS_PER_UPLOAD,
  );
  const chunks = parseChunks(value.chunks, chunkCount);
  const totalBytes = requiredInteger(value, "totalBytes", 0, MAX_REMOTE_BYTES);
  if (chunks.reduce((sum, chunk) => sum + chunk.size, 0) !== totalBytes)
    throw problem("invalid_request");
  const manifest = boundedBase64Url(
    requiredString(
      value,
      "manifest",
      Math.ceil((MAX_MANIFEST_BYTES * 4) / 3) + 4,
    ),
    MAX_MANIFEST_BYTES,
  );
  if (manifest.length < minimumEncryptedPayloadBytes)
    throw problem("invalid_request");
  const manifestDigest = digest(requiredString(value, "manifestSha256", 64));
  if ((await sha256Base64Url(manifest)) !== manifestDigest)
    throw problem("integrity_failed");
  const writerSignature = fixedBase64Url(
    requiredString(value, "writerSignature", 128),
    64,
  );
  if (kind === "object") {
    return {
      uploadId,
      objectId: requiredOpaqueId(value, "objectId"),
      revisionId: requiredOpaqueId(value, "revisionId"),
      baseRevisionId: optionalOpaqueId(value, "baseRevisionId"),
      manifest,
      manifestDigest,
      chunkCount,
      chunks,
      totalBytes,
      cryptoVersion,
      writerSignature,
    };
  }
  const shareExpiresAt = requiredInteger(
    value,
    "expiresAt",
    now + 1,
    now + MAX_SHARE_TTL_MS,
  );
  if (totalBytes > MAX_SHARE_BYTES) throw problem("quota_exceeded");
  return {
    uploadId,
    shareId: requiredOpaqueId(value, "shareId"),
    shareExpiresAt,
    baseRevisionId: null,
    manifest,
    manifestDigest,
    chunkCount,
    chunks,
    totalBytes,
    cryptoVersion,
    writerSignature,
  };
}

function parsePairing(body: Uint8Array): ParsedPairing {
  const value = parseJsonBody(body);
  assertExactKeys(value, [
    "approvalSignature",
    "candidateDeviceId",
    "candidatePairingPublicKey",
    "candidateProof",
    "candidateSigningPublicKey",
    "envelope",
    "expiresAt",
    "pairingNonce",
    "version",
  ]);
  if (
    requiredInteger(value, "version", PROTOCOL_VERSION, PROTOCOL_VERSION) !==
    PROTOCOL_VERSION
  ) {
    throw problem("unsupported_version");
  }
  return {
    candidateDeviceId: requiredOpaqueId(value, "candidateDeviceId"),
    candidateSigningPublicKey: fixedBase64Url(
      requiredString(value, "candidateSigningPublicKey", 64),
      32,
    ),
    candidatePairingPublicKey: fixedBase64Url(
      requiredString(value, "candidatePairingPublicKey", 64),
      32,
    ),
    candidateProof: fixedBase64Url(
      requiredString(value, "candidateProof", 128),
      64,
    ),
    pairingNonce: fixedBase64Url(requiredString(value, "pairingNonce", 32), 16),
    expiresAt: requiredInteger(value, "expiresAt", 1, Number.MAX_SAFE_INTEGER),
    envelope: boundedBase64Url(
      requiredString(
        value,
        "envelope",
        Math.ceil((MAX_MANIFEST_BYTES * 4) / 3) + 4,
      ),
      MAX_MANIFEST_BYTES,
    ),
    approvalSignature: fixedBase64Url(
      requiredString(value, "approvalSignature", 128),
      64,
    ),
  };
}

function parseTombstone(body: Uint8Array): ParsedTombstone {
  const value = parseJsonBody(body);
  assertExactKeys(value, [
    "baseRevisionId",
    "formatVersion",
    "reason",
    "tombstoneRevisionId",
    "writerSignature",
  ]);
  const reason = requiredString(value, "reason", 32);
  if (reason !== "user_request" && reason !== "retention")
    throw problem("invalid_request");
  return {
    tombstoneRevisionId: requiredOpaqueId(value, "tombstoneRevisionId"),
    baseRevisionId: requiredOpaqueId(value, "baseRevisionId"),
    formatVersion: requiredInteger(
      value,
      "formatVersion",
      PROTOCOL_VERSION,
      PROTOCOL_VERSION,
    ),
    reason,
    writerSignature: fixedBase64Url(
      requiredString(value, "writerSignature", 128),
      64,
    ),
  };
}

async function bootstrapDevice(
  request: Request,
  env: Env,
  body: Uint8Array,
  now: number,
): Promise<Response> {
  requireContentType(request, "application/json");
  const value = parseJsonBody(body);
  assertExactKeys(value, [
    "deviceId",
    "pairingPublicKey",
    "selfSignature",
    "signingPublicKey",
    "vaultId",
    "version",
  ]);
  if (
    requiredInteger(value, "version", PROTOCOL_VERSION, PROTOCOL_VERSION) !==
    PROTOCOL_VERSION
  ) {
    throw problem("unsupported_version");
  }
  const vaultId = requiredOpaqueId(value, "vaultId");
  const deviceId = requiredOpaqueId(value, "deviceId");
  const signingPublicKey = fixedBase64Url(
    requiredString(value, "signingPublicKey", 64),
    32,
  );
  const pairingPublicKey = fixedBase64Url(
    requiredString(value, "pairingPublicKey", 64),
    32,
  );
  const selfSignature = fixedBase64Url(
    requiredString(value, "selfSignature", 128),
    64,
  );
  const idempotencyKey = request.headers.get("x-sona-idempotency-key") ?? "";
  if (!isIdempotencyKey(idempotencyKey)) throw problem("invalid_request");
  const providedSecret = request.headers.get("x-sona-bootstrap-secret") ?? "";
  if (!(await equalSecret(providedSecret, env.BOOTSTRAP_SECRET)))
    throw problem("unauthorized");
  const selfSigned = await verifyEd25519(
    signingPublicKey,
    selfSignature,
    canonicalBootstrapBytes({
      audience: env.AUDIENCE,
      vaultId,
      deviceId,
      signingPublicKey,
      pairingPublicKey,
    }),
  );
  if (!selfSigned) throw problem("unauthorized");
  const bodyDigest = await sha256Base64Url(body);
  const target = `bootstrap:${deviceId}`;
  const existing = await idempotencyResult(
    env,
    vaultId,
    deviceId,
    idempotencyKey,
  );
  if (existing !== null) {
    if (
      existing.operation !== "bootstrap" ||
      existing.target !== target ||
      existing.body_digest !== bodyDigest
    ) {
      throw problem("idempotency_conflict");
    }
    const stored = await parseStoredResponse(existing);
    return jsonResponse(stored.body, stored.status, now);
  }
  const response = {
    vault_id: vaultId,
    device_id: deviceId,
    status: "active",
    capabilities: capabilities(),
  };
  const result = await env.DB.batch([
    env.DB.prepare(
      "INSERT OR IGNORE INTO vaults (vault_id, byte_cap, created_at) SELECT ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM bootstrap_registry)",
    ).bind(vaultId, MAX_REMOTE_BYTES, now),
    env.DB.prepare(
      "INSERT INTO devices (vault_id, device_id, signing_public_key, pairing_public_key, status, created_at) SELECT ?, ?, ?, ?, 'active', ? WHERE EXISTS (SELECT 1 FROM vaults WHERE vault_id = ?) AND NOT EXISTS (SELECT 1 FROM devices WHERE vault_id = ?)",
    ).bind(
      vaultId,
      deviceId,
      signingPublicKey,
      pairingPublicKey,
      now,
      vaultId,
      vaultId,
    ),
    env.DB.prepare(
      "UPDATE vaults SET bootstrap_consumed_at = ? WHERE vault_id = ? AND changes() = 1",
    ).bind(now, vaultId),
    env.DB.prepare(
      "INSERT INTO bootstrap_registry (registry_id, vault_id, consumed_at) SELECT 1, ?, ? WHERE changes() = 1",
    ).bind(vaultId, now),
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, 'bootstrap', ?, ?, 201, ?, ? WHERE changes() = 1",
    ).bind(
      vaultId,
      deviceId,
      idempotencyKey,
      target,
      bodyDigest,
      JSON.stringify(response),
      now,
    ),
  ]);
  if (
    changeCount(batchResult(result, 1)) === 1 &&
    changeCount(batchResult(result, 2)) === 1 &&
    changeCount(batchResult(result, 3)) === 1 &&
    changeCount(batchResult(result, 4)) === 1
  ) {
    return jsonResponse(response, 201, now);
  }
  const replay = await idempotencyResult(
    env,
    vaultId,
    deviceId,
    idempotencyKey,
  );
  if (
    replay !== null &&
    replay.operation === "bootstrap" &&
    replay.target === target &&
    replay.body_digest === bodyDigest
  ) {
    const stored = await parseStoredResponse(replay);
    return jsonResponse(stored.body, stored.status, now);
  }
  throw problem("unauthorized");
}

async function pairDevice(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  now: number,
): Promise<Response> {
  requireContentType(request, "application/json");
  const parsed = parsePairing(body);
  if (parsed.expiresAt <= now || parsed.expiresAt > now + 15 * 60 * 1000) {
    throw problem("invalid_request");
  }
  const auth = await authenticate(request, env, route, body, true, now);
  const candidateRecord = canonicalPairCandidateBytes({
    audience: env.AUDIENCE,
    vaultId: auth.vaultId,
    candidateDeviceId: parsed.candidateDeviceId,
    candidateSigningPublicKey: parsed.candidateSigningPublicKey,
    candidatePairingPublicKey: parsed.candidatePairingPublicKey,
    pairingNonce: parsed.pairingNonce,
    expiresAt: parsed.expiresAt,
  });
  const candidateVerified = await verifyEd25519(
    parsed.candidateSigningPublicKey,
    parsed.candidateProof,
    candidateRecord,
  );
  const approvalVerified = await verifyEd25519(
    auth.signingPublicKey,
    parsed.approvalSignature,
    canonicalPairApprovalBytes({
      vaultId: auth.vaultId,
      candidateRecord,
      candidateProof: parsed.candidateProof,
      envelope: parsed.envelope,
    }),
  );
  if (!candidateVerified || !approvalVerified) throw problem("unauthorized");
  const target = `pair:${parsed.candidateDeviceId}`;
  const replay = await existingMutationResponse(
    env,
    auth,
    "pair_device",
    target,
  );
  if (replay !== null) return replay;
  const response = { device_id: parsed.candidateDeviceId, status: "active" };
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const batch = await env.DB.batch([
    env.DB.prepare(
      "INSERT INTO devices (vault_id, device_id, signing_public_key, pairing_public_key, status, created_at) SELECT ?, ?, ?, ?, 'active', ? WHERE NOT EXISTS (SELECT 1 FROM devices WHERE vault_id = ? AND (device_id = ? OR signing_public_key = ? OR pairing_public_key = ?))",
    ).bind(
      auth.vaultId,
      parsed.candidateDeviceId,
      parsed.candidateSigningPublicKey,
      parsed.candidatePairingPublicKey,
      now,
      auth.vaultId,
      parsed.candidateDeviceId,
      parsed.candidateSigningPublicKey,
      parsed.candidatePairingPublicKey,
    ),
    env.DB.prepare(
      "INSERT INTO device_envelopes (vault_id, device_id, envelope, pairing_nonce, protocol_version, created_at) SELECT ?, ?, ?, ?, ?, ? WHERE changes() = 1",
    ).bind(
      auth.vaultId,
      parsed.candidateDeviceId,
      parsed.envelope,
      parsed.pairingNonce,
      PROTOCOL_VERSION,
      now,
    ),
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, 'pair_device', ?, ?, 201, ?, ? WHERE changes() = 1",
    ).bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      target,
      auth.bodyDigest,
      JSON.stringify(response),
      now,
    ),
  ]);
  if (changeCount(batchResult(batch, 0)) === 1)
    return jsonResponse(response, 201, now);
  const duplicate = await existingMutationResponse(
    env,
    auth,
    "pair_device",
    target,
  );
  if (duplicate !== null) return duplicate;
  throw problem("invalid_request");
}

async function listDevices(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const auth = await authenticate(request, env, route, noBody, false, now);
  const devices = await env.DB.prepare(
    "SELECT device_id, signing_public_key, pairing_public_key, status, created_at, revoked_at FROM devices WHERE vault_id = ? ORDER BY created_at, device_id",
  )
    .bind(auth.vaultId)
    .all<{
      created_at: number;
      device_id: string;
      pairing_public_key: ArrayBuffer;
      revoked_at: number | null;
      signing_public_key: ArrayBuffer;
      status: string;
    }>();
  return jsonResponse(
    {
      devices: devices.results.map((device) => ({
        device_id: device.device_id,
        signing_public_key: deviceKeyText(device.signing_public_key),
        pairing_public_key: deviceKeyText(device.pairing_public_key),
        status: device.status,
        created_at: device.created_at,
        revoked_at: device.revoked_at,
      })),
    },
    200,
    now,
  );
}

async function selfDevice(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const auth = await authenticate(request, env, route, noBody, false, now);
  const device = await env.DB.prepare(
    "SELECT d.device_id, d.signing_public_key, d.pairing_public_key, d.status, e.envelope, e.protocol_version FROM devices d LEFT JOIN device_envelopes e ON e.vault_id = d.vault_id AND e.device_id = d.device_id WHERE d.vault_id = ? AND d.device_id = ?",
  )
    .bind(auth.vaultId, auth.deviceId)
    .first<{
      device_id: string;
      envelope: ArrayBuffer | null;
      pairing_public_key: ArrayBuffer;
      protocol_version: number | null;
      signing_public_key: ArrayBuffer;
      status: string;
    }>();
  if (device === null) throw problem("unauthorized");
  return jsonResponse(
    {
      device_id: device.device_id,
      signing_public_key: deviceKeyText(device.signing_public_key),
      pairing_public_key: deviceKeyText(device.pairing_public_key),
      status: device.status,
      envelope:
        device.envelope === null
          ? null
          : base64UrlEncode(bytesFromDb(device.envelope)),
      protocol_version: device.protocol_version,
    },
    200,
    now,
  );
}

async function revokeDevice(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  now: number,
): Promise<Response> {
  methodIs(request, "DELETE");
  requireContentType(request, "application/json");
  const value = parseJsonBody(body);
  assertExactKeys(value, ["reason", "version"]);
  if (
    requiredInteger(value, "version", PROTOCOL_VERSION, PROTOCOL_VERSION) !==
    PROTOCOL_VERSION
  ) {
    throw problem("unsupported_version");
  }
  requiredString(value, "reason", 32);
  const auth = await authenticate(request, env, route, body, true, now);
  const deviceId = route.ids.deviceId;
  if (deviceId === undefined) throw problem("invalid_request");
  const target = `device:${deviceId}`;
  const replay = await existingMutationResponse(
    env,
    auth,
    "revoke_device",
    target,
  );
  if (replay !== null) return replay;
  const [activeCount, targetDevice] = await Promise.all([
    env.DB.prepare(
      "SELECT COUNT(*) AS active_count FROM devices WHERE vault_id = ? AND status = 'active'",
    )
      .bind(auth.vaultId)
      .first<{ active_count: number }>(),
    env.DB.prepare(
      "SELECT status FROM devices WHERE vault_id = ? AND device_id = ?",
    )
      .bind(auth.vaultId, deviceId)
      .first<{ status: string }>(),
  ]);
  if (targetDevice === null) throw problem("not_found");
  if (targetDevice.status !== "active" || activeCount?.active_count === 1)
    throw problem("invalid_request");
  const response = { device_id: deviceId, status: "revoked" };
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const result = await env.DB.batch([
    env.DB.prepare(
      "UPDATE devices SET status = 'revoked', revoked_at = ?, revocation_signature = ? WHERE vault_id = ? AND device_id = ? AND status = 'active' AND (SELECT COUNT(*) FROM devices WHERE vault_id = ? AND status = 'active') > 1",
    ).bind(now, auth.signature, auth.vaultId, deviceId, auth.vaultId),
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, 'revoke_device', ?, ?, 200, ?, ? WHERE changes() = 1",
    ).bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      target,
      auth.bodyDigest,
      JSON.stringify(response),
      now,
    ),
  ]);
  if (changeCount(batchResult(result, 0)) === 1)
    return jsonResponse(response, 200, now);
  const duplicate = await existingMutationResponse(
    env,
    auth,
    "revoke_device",
    target,
  );
  if (duplicate !== null) return duplicate;
  throw problem("invalid_request");
}

async function verifyUploadEnvelope(
  auth: AuthenticatedRequest,
  plan: UploadPlan,
  kind: "object" | "share",
): Promise<void> {
  const verified = await verifyEd25519(
    auth.signingPublicKey,
    plan.writerSignature,
    canonicalUploadEnvelopeBytes({
      vaultId: auth.vaultId,
      kind,
      objectId: plan.objectId ?? null,
      revisionId: plan.revisionId ?? null,
      baseRevisionId: plan.baseRevisionId,
      shareId: plan.shareId ?? null,
      manifestDigest: plan.manifestDigest,
      cryptoVersion: plan.cryptoVersion,
      totalBytes: plan.totalBytes,
      chunks: plan.chunks,
    }),
  );
  if (!verified) throw problem("unauthorized");
  if (kind === "share" && plan.shareId === undefined)
    throw problem("invalid_request");
  if (
    kind === "object" &&
    (plan.objectId === undefined || plan.revisionId === undefined)
  ) {
    throw problem("invalid_request");
  }
}

async function createUpload(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  kind: "object" | "share",
  context: ExecutionContext,
  now: number,
): Promise<Response> {
  methodIs(request, "POST");
  requireContentType(request, "application/json");
  const [planResult, authResult] = await Promise.allSettled([
    parseUploadPlan(body, kind, now),
    authenticateUnclaimed(request, env, route, body, true, now),
  ]);
  if (planResult.status === "rejected") throw planResult.reason;
  if (authResult.status === "rejected") throw authResult.reason;
  const plan = planResult.value;
  const auth = authResult.value;
  await claimMutationNonce(
    env,
    auth.vaultId,
    auth.deviceId,
    auth.nonce,
    now,
    now + NONCE_RETENTION_MS,
  );
  await verifyUploadEnvelope(auth, plan, kind);
  const target =
    kind === "object"
      ? `upload:${plan.uploadId}`
      : `share:${plan.shareId ?? ""}`;
  const operation = kind === "object" ? "create_upload" : "create_share";
  const replay = await existingMutationResponse(env, auth, operation, target);
  if (replay !== null) return replay;
  const response =
    kind === "object"
      ? { upload_id: plan.uploadId, state: "active", accepted_indexes: [] }
      : {
          upload_id: plan.uploadId,
          share_id: plan.shareId,
          state: "pending",
          accepted_indexes: [],
        };
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const expiresAt =
    kind === "share"
      ? Math.min(now + UPLOAD_TTL_MS, plan.shareExpiresAt ?? now)
      : now + UPLOAD_TTL_MS;
  const job = newMaintenanceJob({
    vaultId: auth.vaultId,
    targetId: plan.uploadId,
    jobClass: "reconcile_upload",
    payload: {},
    now,
  });
  const capacityCondition =
    kind === "object"
      ? "(SELECT COUNT(*) FROM upload_sessions WHERE vault_id = ? AND state = 'active') < ?"
      : "(SELECT COUNT(*) FROM upload_sessions WHERE vault_id = ? AND state = 'active') < ? AND (SELECT COUNT(*) FROM shares WHERE vault_id = ? AND state IN ('pending', 'active')) < ?";
  const capacityBindings =
    kind === "object"
      ? [auth.vaultId, MAX_ACTIVE_UPLOADS]
      : [auth.vaultId, MAX_ACTIVE_UPLOADS, auth.vaultId, MAX_ACTIVE_SHARES];
  const reserve = env.DB.prepare(
    `UPDATE vaults SET reserved_bytes = reserved_bytes + ? WHERE vault_id = ? AND used_bytes + reserved_bytes + ? <= byte_cap AND ${capacityCondition}`,
  ).bind(plan.totalBytes, auth.vaultId, plan.totalBytes, ...capacityBindings);
  const session = env.DB.prepare(
    "INSERT INTO upload_sessions (upload_id, vault_id, kind, object_id, revision_id, base_revision_id, share_id, manifest, manifest_digest, chunk_plan, chunk_count, total_bytes, crypto_version, writer_signature, creator_device_id, state, reserved_bytes, expires_at, created_at, updated_at) SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', ?, ?, ?, ? WHERE changes() = 1",
  ).bind(
    plan.uploadId,
    auth.vaultId,
    kind,
    plan.objectId ?? null,
    plan.revisionId ?? null,
    plan.baseRevisionId,
    plan.shareId ?? null,
    plan.manifest,
    plan.manifestDigest,
    planToJson(plan.chunks),
    plan.chunkCount,
    plan.totalBytes,
    plan.cryptoVersion,
    plan.writerSignature,
    auth.deviceId,
    plan.totalBytes,
    expiresAt,
    now,
    now,
  );
  const statements: D1PreparedStatement[] = [reserve, session];
  if (kind === "share") {
    const shareId = plan.shareId;
    const expiresAtValue = plan.shareExpiresAt;
    if (shareId === undefined || expiresAtValue === undefined)
      throw problem("invalid_request");
    statements.push(
      env.DB.prepare(
        "INSERT INTO shares (vault_id, share_id, upload_id, state, manifest_digest, chunk_count, total_bytes, crypto_version, writer_signature, expires_at, created_at) SELECT ?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?, ? WHERE changes() = 1",
      ).bind(
        auth.vaultId,
        shareId,
        plan.uploadId,
        plan.manifestDigest,
        plan.chunkCount,
        plan.totalBytes,
        plan.cryptoVersion,
        plan.writerSignature,
        expiresAtValue,
        now,
      ),
    );
  }
  statements.push(
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, ?, ?, ?, 201, ?, ? WHERE EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'active')",
    ).bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      operation,
      target,
      auth.bodyDigest,
      JSON.stringify(response),
      now,
      plan.uploadId,
    ),
    env.DB.prepare(
      "INSERT INTO maintenance_jobs (job_id, vault_id, job_class, target_id, payload_json, state, next_attempt_at, created_at, updated_at) SELECT ?, ?, 'reconcile_upload', ?, '{}', 'queued', ?, ?, ? WHERE EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'active')",
    ).bind(
      job.jobId,
      auth.vaultId,
      plan.uploadId,
      now,
      now,
      now,
      plan.uploadId,
    ),
  );
  const results = await env.DB.batch(statements);
  if (changeCount(batchResult(results, 0)) === 0)
    throw problem("quota_exceeded");
  scheduleMaintenance(env, context, job.jobId);
  return jsonResponse(response, 201, now);
}

async function uploadStatus(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const auth = await authenticate(request, env, route, noBody, false, now);
  const uploadId = route.ids.uploadId;
  if (uploadId === undefined) throw problem("invalid_request");
  const session = await uploadSession(env, auth.vaultId, uploadId);
  if (session === null) throw problem("not_found");
  const accepted = await env.DB.prepare(
    "SELECT chunk_index FROM upload_chunks WHERE upload_id = ? ORDER BY chunk_index",
  )
    .bind(uploadId)
    .all<{ chunk_index: number }>();
  return jsonResponse(
    {
      upload_id: uploadId,
      state: session.state,
      accepted_indexes: accepted.results.map((chunk) => chunk.chunk_index),
      expires_at: session.expires_at,
      committed_sequence: session.committed_sequence,
    },
    200,
    now,
  );
}

function activeUploadForWriter(
  session: UploadSessionRow | null,
  auth: AuthenticatedRequest,
  now: number,
): UploadSessionRow {
  if (session === null) throw problem("not_found");
  if (session.creator_device_id !== auth.deviceId)
    throw problem("unauthorized");
  if (session.state !== "active" || session.expires_at <= now)
    throw problem("upload_incomplete");
  return session;
}

function expectedChunk(
  session: UploadSessionRow,
  index: number,
): UploadPlanChunk {
  const plan = planFromJson(session.chunk_plan);
  const chunk = plan[index];
  if (chunk === undefined || chunk.index !== index)
    throw problem("invalid_request");
  return chunk;
}

function matchingR2Object(
  stored: R2Object,
  expected: UploadPlanChunk,
): boolean {
  const checksum = stored.checksums.sha256;
  const expectedDigest = base64UrlDecode(expected.sha256);
  return (
    expectedDigest !== null &&
    stored.size === expected.size &&
    stored.customMetadata?.sha256 === expected.sha256 &&
    checksum !== undefined &&
    equalBytes(new Uint8Array(checksum), expectedDigest)
  );
}

async function putUploadChunk(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  now: number,
): Promise<Response> {
  methodIs(request, "PUT");
  requireContentType(request, "application/octet-stream");
  const uploadId = route.ids.uploadId;
  const indexText = route.ids.index;
  if (uploadId === undefined || indexText === undefined)
    throw problem("invalid_request");
  const declaredDigest = digest(
    requiredString(
      { digest: request.headers.get("x-sona-chunk-sha256") ?? "" },
      "digest",
      64,
    ),
  );
  const auth = await authenticate(request, env, route, body, true, now);
  const target = `upload_chunk:${uploadId}:${indexText}`;
  const replay = await existingMutationResponse(
    env,
    auth,
    "put_upload_chunk",
    target,
  );
  if (replay !== null) return replay;
  const index = Number(indexText);
  const session = activeUploadForWriter(
    await uploadSession(env, auth.vaultId, uploadId),
    auth,
    now,
  );
  const expected = expectedChunk(session, index);
  if (
    body.length !== expected.size ||
    auth.bodyDigest !== expected.sha256 ||
    declaredDigest !== expected.sha256
  ) {
    throw problem("integrity_failed");
  }
  const digestBytes = base64UrlDecode(expected.sha256);
  if (digestBytes === null) throw problem("integrity_failed");
  const key = r2ChunkKey(auth.vaultId, uploadId, index);
  let stored: R2Object | null;
  try {
    stored = await env.CIPHERTEXT.put(key, body, {
      onlyIf: { etagDoesNotMatch: "*" },
      httpMetadata: { contentType: "application/octet-stream" },
      customMetadata: { sha256: expected.sha256 },
      sha256: digestBytes,
    });
    if (stored === null) stored = await env.CIPHERTEXT.head(key);
  } catch {
    throw dependencyProblem("r2");
  }
  if (stored === null || !matchingR2Object(stored, expected))
    throw problem("chunk_conflict");
  const response = { upload_id: uploadId, index, accepted: true };
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  await env.DB.batch([
    env.DB.prepare(
      "INSERT OR IGNORE INTO upload_chunks (upload_id, chunk_index, byte_size, digest, r2_key, accepted_at) SELECT ?, ?, ?, ?, ?, ? WHERE EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'active' AND expires_at > ?)",
    ).bind(
      uploadId,
      index,
      expected.size,
      expected.sha256,
      key,
      now,
      uploadId,
      now,
    ),
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, 'put_upload_chunk', ?, ?, 200, ?, ? WHERE EXISTS (SELECT 1 FROM upload_chunks WHERE upload_id = ? AND chunk_index = ? AND byte_size = ? AND digest = ?)",
    ).bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      target,
      auth.bodyDigest,
      JSON.stringify(response),
      now,
      uploadId,
      index,
      expected.size,
      expected.sha256,
    ),
  ]);
  const receipt = await env.DB.prepare(
    "SELECT byte_size, digest FROM upload_chunks WHERE upload_id = ? AND chunk_index = ?",
  )
    .bind(uploadId, index)
    .first<{ byte_size: number; digest: string }>();
  if (receipt === null) throw problem("upload_incomplete");
  if (receipt.byte_size !== expected.size || receipt.digest !== expected.sha256)
    throw problem("chunk_conflict");
  return jsonResponse(response, 200, now);
}

function parseVersionOnly(body: Uint8Array): void {
  const value = parseJsonBody(body);
  assertExactKeys(value, ["version"]);
  if (
    requiredInteger(value, "version", PROTOCOL_VERSION, PROTOCOL_VERSION) !==
    PROTOCOL_VERSION
  ) {
    throw problem("unsupported_version");
  }
}

async function assertCompleteUpload(
  session: UploadSessionRow,
  env: Env,
): Promise<UploadPlanChunk[]> {
  const plan = planFromJson(session.chunk_plan);
  if (plan.length !== session.chunk_count) throw problem("integrity_failed");
  const receipts = await env.DB.prepare(
    "SELECT chunk_index, byte_size, digest, r2_key FROM upload_chunks WHERE upload_id = ? ORDER BY chunk_index",
  )
    .bind(session.upload_id)
    .all<{
      byte_size: number;
      chunk_index: number;
      digest: string;
      r2_key: string;
    }>();
  if (receipts.results.length !== plan.length)
    throw problem("upload_incomplete");
  const verifiedChunks = plan.map((expected, index) => {
    const receipt = receipts.results[index];
    if (
      receipt === undefined ||
      receipt.chunk_index !== expected.index ||
      receipt.byte_size !== expected.size ||
      receipt.digest !== expected.sha256
    ) {
      throw problem("integrity_failed");
    }
    return { expected, receipt };
  });
  await forEachBounded(
    verifiedChunks,
    MAX_CONCURRENT_R2_READS,
    async ({ expected, receipt }) => {
      let stored: R2Object | null;
      try {
        stored = await env.CIPHERTEXT.head(receipt.r2_key);
      } catch {
        throw dependencyProblem("r2");
      }
      if (stored === null || !matchingR2Object(stored, expected))
        throw problem("integrity_failed");
    },
  );
  return plan;
}

async function committedMutationResponse(
  env: Env,
  auth: AuthenticatedRequest,
): Promise<Response> {
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const stored = await idempotencyResult(
    env,
    auth.vaultId,
    auth.deviceId,
    idempotencyKey,
  );
  if (stored === null) throw problem("integrity_failed");
  const response = await parseStoredResponse(stored);
  return jsonResponse(response.body, response.status);
}

async function commitObjectUpload(
  env: Env,
  auth: AuthenticatedRequest,
  session: UploadSessionRow,
  target: string,
  now: number,
): Promise<Response> {
  if (session.object_id === null || session.revision_id === null)
    throw problem("integrity_failed");
  const plan = await assertCompleteUpload(session, env);
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const results = await env.DB.batch([
    env.DB.prepare(
      "INSERT INTO revisions (vault_id, object_id, revision_id, parent_revision_id, manifest, manifest_digest, manifest_size, chunk_count, total_bytes, crypto_version, writer_signature, writer_device_id, tombstone, tombstone_reason, created_at) SELECT s.vault_id, s.object_id, s.revision_id, s.base_revision_id, s.manifest, s.manifest_digest, length(s.manifest), s.chunk_count, s.total_bytes, s.crypto_version, s.writer_signature, s.creator_device_id, 0, NULL, ? FROM upload_sessions s WHERE s.upload_id = ? AND s.state = 'active' AND ((s.base_revision_id IS NULL AND NOT EXISTS (SELECT 1 FROM object_heads h WHERE h.vault_id = s.vault_id AND h.object_id = s.object_id)) OR (s.base_revision_id IS NOT NULL AND EXISTS (SELECT 1 FROM object_heads h WHERE h.vault_id = s.vault_id AND h.object_id = s.object_id AND h.head_revision_id = s.base_revision_id)))",
    ).bind(now, session.upload_id),
    env.DB.prepare(
      "INSERT INTO revision_chunks (vault_id, object_id, revision_id, chunk_index, r2_key, byte_size, digest) SELECT s.vault_id, s.object_id, s.revision_id, uc.chunk_index, uc.r2_key, uc.byte_size, uc.digest FROM upload_sessions s JOIN upload_chunks uc ON uc.upload_id = s.upload_id WHERE s.upload_id = ? AND s.state = 'active' AND EXISTS (SELECT 1 FROM revisions r WHERE r.vault_id = s.vault_id AND r.object_id = s.object_id AND r.revision_id = s.revision_id)",
    ).bind(session.upload_id),
    env.DB.prepare(
      "INSERT OR IGNORE INTO object_heads (vault_id, object_id, head_revision_id, tombstone, updated_at) SELECT s.vault_id, s.object_id, s.revision_id, 0, ? FROM upload_sessions s WHERE s.upload_id = ? AND s.state = 'active' AND s.base_revision_id IS NULL AND EXISTS (SELECT 1 FROM revisions r WHERE r.vault_id = s.vault_id AND r.object_id = s.object_id AND r.revision_id = s.revision_id)",
    ).bind(now, session.upload_id),
    env.DB.prepare(
      "UPDATE object_heads SET head_revision_id = (SELECT revision_id FROM upload_sessions WHERE upload_id = ?), tombstone = 0, updated_at = ? WHERE vault_id = (SELECT vault_id FROM upload_sessions WHERE upload_id = ?) AND object_id = (SELECT object_id FROM upload_sessions WHERE upload_id = ?) AND head_revision_id = (SELECT base_revision_id FROM upload_sessions WHERE upload_id = ?) AND (SELECT state FROM upload_sessions WHERE upload_id = ?) = 'active' AND EXISTS (SELECT 1 FROM revisions WHERE vault_id = object_heads.vault_id AND object_id = object_heads.object_id AND revision_id = (SELECT revision_id FROM upload_sessions WHERE upload_id = ?))",
    ).bind(
      session.upload_id,
      now,
      session.upload_id,
      session.upload_id,
      session.upload_id,
      session.upload_id,
      session.upload_id,
    ),
    env.DB.prepare(
      "UPDATE upload_sessions SET state = 'committed', committed_sequence = (SELECT next_change_sequence FROM vaults WHERE vault_id = ?), updated_at = ? WHERE upload_id = ? AND state = 'active' AND EXISTS (SELECT 1 FROM object_heads WHERE vault_id = ? AND object_id = ? AND head_revision_id = ?)",
    ).bind(
      auth.vaultId,
      now,
      session.upload_id,
      auth.vaultId,
      session.object_id,
      session.revision_id,
    ),
    env.DB.prepare(
      "UPDATE vaults SET reserved_bytes = reserved_bytes - ?, used_bytes = used_bytes + ? WHERE vault_id = ? AND changes() = 1",
    ).bind(session.reserved_bytes, session.total_bytes, auth.vaultId),
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, 'commit_upload', ?, ?, 200, json_object('upload_id', ?, 'state', 'committed', 'revision_id', ?, 'change_sequence', (SELECT next_change_sequence FROM vaults WHERE vault_id = ?)), ? WHERE EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'committed')",
    ).bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      target,
      auth.bodyDigest,
      session.upload_id,
      session.revision_id,
      auth.vaultId,
      now,
      session.upload_id,
    ),
  ]);
  const headChanges =
    changeCount(batchResult(results, 2)) + changeCount(batchResult(results, 3));
  if (
    changeCount(batchResult(results, 0)) < 1 ||
    changeCount(batchResult(results, 1)) < plan.length ||
    headChanges < 1 ||
    changeCount(batchResult(results, 4)) < 1 ||
    changeCount(batchResult(results, 5)) < 1 ||
    changeCount(batchResult(results, 6)) < 1
  ) {
    throw problem("stale_revision");
  }
  const completed = await uploadSession(env, auth.vaultId, session.upload_id);
  if (completed?.state !== "committed") throw problem("stale_revision");
  return committedMutationResponse(env, auth);
}

async function commitShareUpload(
  env: Env,
  auth: AuthenticatedRequest,
  session: UploadSessionRow,
  target: string,
  now: number,
): Promise<Response> {
  if (session.share_id === null) throw problem("integrity_failed");
  await assertCompleteUpload(session, env);
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const results = await env.DB.batch([
    env.DB.prepare(
      "UPDATE shares SET state = 'active' WHERE vault_id = ? AND share_id = ? AND state = 'pending' AND expires_at > ? AND EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'active')",
    ).bind(auth.vaultId, session.share_id, now, session.upload_id),
    env.DB.prepare(
      "UPDATE upload_sessions SET state = 'committed', updated_at = ? WHERE upload_id = ? AND state = 'active' AND EXISTS (SELECT 1 FROM shares WHERE vault_id = ? AND share_id = ? AND state = 'active')",
    ).bind(now, session.upload_id, auth.vaultId, session.share_id),
    env.DB.prepare(
      "UPDATE vaults SET reserved_bytes = reserved_bytes - ?, used_bytes = used_bytes + ? WHERE vault_id = ? AND changes() = 1",
    ).bind(session.reserved_bytes, session.total_bytes, auth.vaultId),
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, 'commit_upload', ?, ?, 200, json_object('upload_id', ?, 'share_id', ?, 'state', 'active'), ? WHERE EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'committed')",
    ).bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      target,
      auth.bodyDigest,
      session.upload_id,
      session.share_id,
      now,
      session.upload_id,
    ),
  ]);
  if (
    changeCount(batchResult(results, 0)) < 1 ||
    changeCount(batchResult(results, 1)) < 1 ||
    changeCount(batchResult(results, 2)) < 1 ||
    changeCount(batchResult(results, 3)) < 1
  ) {
    throw problem("upload_incomplete");
  }
  const completed = await uploadSession(env, auth.vaultId, session.upload_id);
  if (completed?.state !== "committed") throw problem("upload_incomplete");
  return committedMutationResponse(env, auth);
}

async function commitUpload(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  now: number,
): Promise<Response> {
  methodIs(request, "POST");
  requireContentType(request, "application/json");
  parseVersionOnly(body);
  const auth = await authenticate(request, env, route, body, true, now);
  const uploadId = route.ids.uploadId;
  if (uploadId === undefined) throw problem("invalid_request");
  const target = `upload:${uploadId}`;
  const replay = await existingMutationResponse(
    env,
    auth,
    "commit_upload",
    target,
  );
  if (replay !== null) return replay;
  const session = activeUploadForWriter(
    await uploadSession(env, auth.vaultId, uploadId),
    auth,
    now,
  );
  if (session.kind === "object")
    return commitObjectUpload(env, auth, session, target, now);
  return commitShareUpload(env, auth, session, target, now);
}

async function cancelUpload(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  context: ExecutionContext,
  now: number,
): Promise<Response> {
  methodIs(request, "DELETE");
  requireContentType(request, "application/json");
  parseVersionOnly(body);
  const auth = await authenticate(request, env, route, body, true, now);
  const uploadId = route.ids.uploadId;
  if (uploadId === undefined) throw problem("invalid_request");
  const target = `upload:${uploadId}`;
  const replay = await existingMutationResponse(
    env,
    auth,
    "cancel_upload",
    target,
  );
  if (replay !== null) return replay;
  const session = activeUploadForWriter(
    await uploadSession(env, auth.vaultId, uploadId),
    auth,
    now,
  );
  const response = { upload_id: uploadId, state: "cancelled" };
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const job = newMaintenanceJob({
    vaultId: auth.vaultId,
    targetId: uploadId,
    jobClass: "cleanup_upload",
    payload: {},
    now,
  });
  const results = await env.DB.batch([
    env.DB.prepare(
      "UPDATE upload_sessions SET state = 'cancelled', updated_at = ? WHERE upload_id = ? AND state = 'active'",
    ).bind(now, uploadId),
    env.DB.prepare(
      "UPDATE vaults SET reserved_bytes = reserved_bytes - ?, used_bytes = used_bytes + ? WHERE vault_id = ? AND changes() = 1",
    ).bind(session.reserved_bytes, session.reserved_bytes, auth.vaultId),
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, 'cancel_upload', ?, ?, 200, ?, ? WHERE EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'cancelled')",
    ).bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      target,
      auth.bodyDigest,
      JSON.stringify(response),
      now,
      uploadId,
    ),
    env.DB.prepare(
      "INSERT INTO maintenance_jobs (job_id, vault_id, job_class, target_id, payload_json, state, next_attempt_at, created_at, updated_at) SELECT ?, ?, 'cleanup_upload', ?, '{}', 'queued', ?, ?, ? WHERE EXISTS (SELECT 1 FROM upload_sessions WHERE upload_id = ? AND state = 'cancelled')",
    ).bind(job.jobId, auth.vaultId, uploadId, now, now, now, uploadId),
  ]);
  if (changeCount(batchResult(results, 0)) === 0)
    throw problem("upload_incomplete");
  scheduleMaintenance(env, context, job.jobId);
  return jsonResponse(response, 200, now);
}

async function changesPage(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const auth = await authenticate(request, env, route, noBody, false, now);
  const url = new URL(request.url);
  const after = decodeChangeCursor(url.searchParams.get("cursor"));
  const limit = parseLimit(url.searchParams.get("limit"), 50, MAX_CHANGE_PAGE);
  const vault = await env.DB.prepare(
    "SELECT min_change_sequence, next_change_sequence FROM vaults WHERE vault_id = ?",
  )
    .bind(auth.vaultId)
    .first<{ min_change_sequence: number; next_change_sequence: number }>();
  if (vault === null) throw problem("unauthorized");
  if (after < vault.min_change_sequence - 1) throw problem("cursor_expired");
  const rows = await env.DB.prepare(
    "SELECT sequence, object_id, revision_id, tombstone FROM changes WHERE vault_id = ? AND sequence > ? ORDER BY sequence LIMIT ?",
  )
    .bind(auth.vaultId, after, limit + 1)
    .all<{
      object_id: string;
      revision_id: string;
      sequence: number;
      tombstone: number;
    }>();
  const hasMore = rows.results.length > limit;
  const page = rows.results.slice(0, limit);
  const lastSequence = page.at(-1)?.sequence ?? after;
  return jsonResponse(
    {
      changes: page.map((change) => ({
        sequence: change.sequence,
        object_id: change.object_id,
        revision_id: change.revision_id,
        tombstone: change.tombstone === 1,
      })),
      next_cursor: encodeChangeCursor(lastSequence),
      has_more: hasMore,
      high_water: vault.next_change_sequence,
    },
    200,
    now,
  );
}

async function snapshotPage(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const auth = await authenticate(request, env, route, noBody, false, now);
  const url = new URL(request.url);
  const suppliedHighWater = url.searchParams.get("highWater");
  if (suppliedHighWater === null && url.searchParams.has("after"))
    throw problem("invalid_request");
  const vault = await env.DB.prepare(
    "SELECT next_change_sequence FROM vaults WHERE vault_id = ?",
  )
    .bind(auth.vaultId)
    .first<{ next_change_sequence: number }>();
  if (vault === null) throw problem("unauthorized");
  const highWater =
    suppliedHighWater === null
      ? vault.next_change_sequence
      : decodeSnapshotHighWater(suppliedHighWater);
  if (highWater > vault.next_change_sequence) throw problem("invalid_request");
  const after = decodeSnapshotAfter(url.searchParams.get("after"), highWater);
  const limit = parseLimit(
    url.searchParams.get("limit"),
    50,
    MAX_SNAPSHOT_PAGE,
  );
  const rows = await env.DB.prepare(
    "SELECT object_id, head_revision_id, tombstone, head_sequence FROM object_heads WHERE vault_id = ? AND head_sequence <= ? AND object_id > ? ORDER BY object_id LIMIT ?",
  )
    .bind(auth.vaultId, highWater, after, limit + 1)
    .all<{
      head_revision_id: string;
      head_sequence: number;
      object_id: string;
      tombstone: number;
    }>();
  const hasMore = rows.results.length > limit;
  const page = rows.results.slice(0, limit);
  const lastId = page.at(-1)?.object_id;
  return jsonResponse(
    {
      heads: page.map((head) => ({
        object_id: head.object_id,
        revision_id: head.head_revision_id,
        tombstone: head.tombstone === 1,
        sequence: head.head_sequence,
      })),
      high_water: encodeSnapshotHighWater(highWater),
      after:
        lastId === undefined ? null : encodeSnapshotAfter(highWater, lastId),
      has_more: hasMore,
    },
    200,
    now,
  );
}

async function readableRevision(
  env: Env,
  vaultId: string,
  objectId: string,
  revisionId: string,
): Promise<{
  chunk_count: number;
  crypto_version: number;
  manifest: ArrayBuffer;
  manifest_digest: string;
  object_id: string;
  parent_revision_id: string | null;
  revision_id: string;
  total_bytes: number;
  writer_device_id: string;
  writer_signature: ArrayBuffer;
}> {
  const head = await objectHead(env, vaultId, objectId);
  if (head === null || head.tombstone === 1) throw problem("not_found");
  const revision = await env.DB.prepare(
    "SELECT object_id, revision_id, parent_revision_id, manifest, manifest_digest, chunk_count, total_bytes, crypto_version, writer_signature, writer_device_id, tombstone FROM revisions WHERE vault_id = ? AND object_id = ? AND revision_id = ?",
  )
    .bind(vaultId, objectId, revisionId)
    .first<{
      chunk_count: number;
      crypto_version: number;
      manifest: ArrayBuffer;
      manifest_digest: string;
      object_id: string;
      parent_revision_id: string | null;
      revision_id: string;
      tombstone: number;
      total_bytes: number;
      writer_device_id: string;
      writer_signature: ArrayBuffer;
    }>();
  if (revision === null || revision.tombstone === 1) throw problem("not_found");
  return revision;
}

async function revisionManifest(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const auth = await authenticate(request, env, route, noBody, false, now);
  const objectId = route.ids.objectId;
  const revisionId = route.ids.revisionId;
  if (objectId === undefined || revisionId === undefined)
    throw problem("invalid_request");
  const revision = await readableRevision(
    env,
    auth.vaultId,
    objectId,
    revisionId,
  );
  return jsonResponse(
    {
      envelope: {
        object_id: revision.object_id,
        revision_id: revision.revision_id,
        parent_revision_id: revision.parent_revision_id,
        manifest_sha256: revision.manifest_digest,
        chunk_count: revision.chunk_count,
        total_bytes: revision.total_bytes,
        crypto_version: revision.crypto_version,
        writer_device_id: revision.writer_device_id,
        writer_signature: base64UrlEncode(
          bytesFromDb(revision.writer_signature),
        ),
      },
      manifest: base64UrlEncode(bytesFromDb(revision.manifest)),
    },
    200,
    now,
  );
}

async function revisionChunk(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const auth = await authenticate(request, env, route, noBody, false, now);
  const objectId = route.ids.objectId;
  const revisionId = route.ids.revisionId;
  const indexText = route.ids.index;
  if (
    objectId === undefined ||
    revisionId === undefined ||
    indexText === undefined
  ) {
    throw problem("invalid_request");
  }
  await readableRevision(env, auth.vaultId, objectId, revisionId);
  const chunk = await env.DB.prepare(
    "SELECT r2_key, byte_size, digest FROM revision_chunks WHERE vault_id = ? AND object_id = ? AND revision_id = ? AND chunk_index = ? AND purged_at IS NULL",
  )
    .bind(auth.vaultId, objectId, revisionId, Number(indexText))
    .first<{ byte_size: number; digest: string; r2_key: string }>();
  if (chunk === null) throw problem("not_found");
  let stored: R2ObjectBody | null;
  try {
    stored = await env.CIPHERTEXT.get(chunk.r2_key);
  } catch {
    throw dependencyProblem("r2");
  }
  if (stored === null) throw dependencyProblem("r2");
  const expected: UploadPlanChunk = {
    index: Number(indexText),
    size: chunk.byte_size,
    sha256: chunk.digest,
  };
  if (!matchingR2Object(stored, expected)) throw problem("integrity_failed");
  return bytesResponse(
    stored.body,
    200,
    { ETag: responseDigestHeader(chunk.digest) },
    now,
  );
}

async function deleteObject(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  context: ExecutionContext,
  now: number,
): Promise<Response> {
  methodIs(request, "DELETE");
  requireContentType(request, "application/json");
  const parsed = parseTombstone(body);
  const auth = await authenticate(request, env, route, body, true, now);
  const objectId = route.ids.objectId;
  if (objectId === undefined) throw problem("invalid_request");
  const signed = await verifyEd25519(
    auth.signingPublicKey,
    parsed.writerSignature,
    canonicalTombstoneBytes({
      vaultId: auth.vaultId,
      objectId,
      tombstoneRevisionId: parsed.tombstoneRevisionId,
      baseRevisionId: parsed.baseRevisionId,
      reason: parsed.reason,
      formatVersion: parsed.formatVersion,
    }),
  );
  if (!signed) throw problem("unauthorized");
  const target = `object:${objectId}`;
  const replay = await existingMutationResponse(
    env,
    auth,
    "tombstone_object",
    target,
  );
  if (replay !== null) return replay;
  const previous = await objectHead(env, auth.vaultId, objectId);
  if (previous === null) throw problem("not_found");
  const existingRevision = await env.DB.prepare(
    "SELECT revision_id FROM revisions WHERE vault_id = ? AND object_id = ? AND revision_id = ?",
  )
    .bind(auth.vaultId, objectId, parsed.tombstoneRevisionId)
    .first<{ revision_id: string }>();
  if (existingRevision !== null) throw problem("invalid_request");
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const job = newMaintenanceJob({
    vaultId: auth.vaultId,
    targetId: objectId,
    jobClass: "cleanup_object",
    payload: { tombstone_revision_id: parsed.tombstoneRevisionId },
    now,
  });
  const results = await env.DB.batch([
    env.DB.prepare(
      "INSERT INTO revisions (vault_id, object_id, revision_id, parent_revision_id, manifest, manifest_digest, manifest_size, chunk_count, total_bytes, crypto_version, writer_signature, writer_device_id, tombstone, tombstone_reason, created_at) SELECT ?, ?, ?, ?, ?, ?, 0, 0, 0, ?, ?, ?, 1, ?, ? WHERE EXISTS (SELECT 1 FROM object_heads WHERE vault_id = ? AND object_id = ? AND head_revision_id = ?)",
    ).bind(
      auth.vaultId,
      objectId,
      parsed.tombstoneRevisionId,
      parsed.baseRevisionId,
      noBody,
      emptyDigest,
      parsed.formatVersion,
      parsed.writerSignature,
      auth.deviceId,
      parsed.reason,
      now,
      auth.vaultId,
      objectId,
      parsed.baseRevisionId,
    ),
    env.DB.prepare(
      "UPDATE object_heads SET head_revision_id = ?, tombstone = 1, updated_at = ? WHERE vault_id = ? AND object_id = ? AND head_revision_id = ? AND EXISTS (SELECT 1 FROM revisions WHERE vault_id = ? AND object_id = ? AND revision_id = ? AND tombstone = 1)",
    ).bind(
      parsed.tombstoneRevisionId,
      now,
      auth.vaultId,
      objectId,
      parsed.baseRevisionId,
      auth.vaultId,
      objectId,
      parsed.tombstoneRevisionId,
    ),
    env.DB.prepare(
      "INSERT INTO maintenance_jobs (job_id, vault_id, job_class, target_id, payload_json, state, next_attempt_at, created_at, updated_at) SELECT ?, ?, 'cleanup_object', ?, ?, 'queued', ?, ?, ? WHERE EXISTS (SELECT 1 FROM object_heads WHERE vault_id = ? AND object_id = ? AND head_revision_id = ? AND tombstone = 1)",
    ).bind(
      job.jobId,
      auth.vaultId,
      objectId,
      JSON.stringify(job.payload),
      now,
      now,
      now,
      auth.vaultId,
      objectId,
      parsed.tombstoneRevisionId,
    ),
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, 'tombstone_object', ?, ?, 200, json_object('object_id', ?, 'revision_id', ?, 'tombstone', json('true'), 'change_sequence', (SELECT next_change_sequence FROM vaults WHERE vault_id = ?)), ? WHERE EXISTS (SELECT 1 FROM object_heads WHERE vault_id = ? AND object_id = ? AND head_revision_id = ? AND tombstone = 1)",
    ).bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      target,
      auth.bodyDigest,
      objectId,
      parsed.tombstoneRevisionId,
      auth.vaultId,
      now,
      auth.vaultId,
      objectId,
      parsed.tombstoneRevisionId,
    ),
  ]);
  if (
    changeCount(batchResult(results, 0)) < 1 ||
    changeCount(batchResult(results, 1)) < 1 ||
    changeCount(batchResult(results, 2)) < 1 ||
    changeCount(batchResult(results, 3)) < 1
  ) {
    throw problem("stale_revision");
  }
  const head = await objectHead(env, auth.vaultId, objectId);
  if (
    head?.head_revision_id !== parsed.tombstoneRevisionId ||
    head.tombstone !== 1
  ) {
    throw problem("stale_revision");
  }
  scheduleMaintenance(env, context, job.jobId);
  return committedMutationResponse(env, auth);
}

interface PublicShare {
  chunk_count: number;
  crypto_version: number;
  expires_at: number;
  manifest: ArrayBuffer;
  manifest_digest: string;
  share_id: string;
  total_bytes: number;
  upload_id: string;
  vault_id: string;
  writer_signature: ArrayBuffer;
}

async function activePublicShare(
  env: Env,
  shareId: string,
  now: number,
): Promise<PublicShare> {
  const share = await env.DB.prepare(
    "SELECT s.vault_id, s.share_id, s.upload_id, s.manifest_digest, s.chunk_count, s.total_bytes, s.crypto_version, s.writer_signature, s.expires_at, u.manifest FROM shares s JOIN upload_sessions u ON u.upload_id = s.upload_id WHERE s.share_id = ? AND s.state = 'active' AND s.expires_at > ? AND u.state = 'committed'",
  )
    .bind(shareId, now)
    .first<PublicShare>();
  if (share === null) throw problem("not_found");
  return share;
}

async function shareManifest(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const shareId = route.ids.shareId;
  if (shareId === undefined) throw problem("not_found");
  const share = await activePublicShare(env, shareId, now);
  const session = await uploadSession(env, share.vault_id, share.upload_id);
  if (session === null) throw problem("not_found");
  const chunks = planFromJson(session.chunk_plan);
  return jsonResponse(
    {
      version: PROTOCOL_VERSION,
      share: {
        share_id: share.share_id,
        crypto_version: share.crypto_version,
        manifest_sha256: share.manifest_digest,
        chunk_count: share.chunk_count,
        total_bytes: share.total_bytes,
        writer_signature: base64UrlEncode(bytesFromDb(share.writer_signature)),
      },
      manifest: base64UrlEncode(bytesFromDb(share.manifest)),
      chunks,
    },
    200,
    now,
  );
}

async function shareChunk(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const shareId = route.ids.shareId;
  const indexText = route.ids.index;
  if (shareId === undefined || indexText === undefined)
    throw problem("not_found");
  const share = await activePublicShare(env, shareId, now);
  const index = Number(indexText);
  const session = await uploadSession(env, share.vault_id, share.upload_id);
  if (session === null) throw problem("not_found");
  const expected = expectedChunk(session, index);
  const receipt = await env.DB.prepare(
    "SELECT r2_key, byte_size, digest FROM upload_chunks WHERE upload_id = ? AND chunk_index = ?",
  )
    .bind(session.upload_id, index)
    .first<{ byte_size: number; digest: string; r2_key: string }>();
  if (
    receipt === null ||
    receipt.byte_size !== expected.size ||
    receipt.digest !== expected.sha256
  ) {
    throw problem("not_found");
  }
  let stored: R2ObjectBody | null;
  try {
    stored = await env.CIPHERTEXT.get(receipt.r2_key);
  } catch {
    throw problem("not_found");
  }
  if (stored === null || !matchingR2Object(stored, expected))
    throw problem("not_found");
  return bytesResponse(
    stored.body,
    200,
    { ETag: responseDigestHeader(expected.sha256) },
    now,
  );
}

function encryptedShareFile(
  env: Env,
  shareId: string,
  initial: PublicShare,
  session: UploadSessionRow,
): ReadableStream<Uint8Array> {
  const plan = planFromJson(session.chunk_plan);
  const header = utf8(
    `${JSON.stringify({
      format: "sona-encrypted-share-v1",
      version: PROTOCOL_VERSION,
      share: {
        share_id: initial.share_id,
        crypto_version: initial.crypto_version,
        manifest_sha256: initial.manifest_digest,
        chunk_count: initial.chunk_count,
        total_bytes: initial.total_bytes,
        writer_signature: base64UrlEncode(
          bytesFromDb(initial.writer_signature),
        ),
      },
      manifest: base64UrlEncode(bytesFromDb(initial.manifest)),
      chunks: plan,
    })}\n`,
  );
  let phase: "header" | "chunk" | "done" = "header";
  let index = 0;
  let reader: ReadableStreamDefaultReader<Uint8Array> | null = null;
  let prefixed = false;
  return new ReadableStream<Uint8Array>({
    async pull(controller): Promise<void> {
      if (phase === "header") {
        controller.enqueue(header);
        phase = "chunk";
        return;
      }
      if (phase === "done") {
        controller.close();
        return;
      }
      if (reader !== null) {
        const next = await reader.read();
        if (!next.done) {
          controller.enqueue(next.value);
          return;
        }
        reader.releaseLock();
        reader = null;
        prefixed = false;
        index += 1;
        return;
      }
      if (index >= plan.length) {
        phase = "done";
        controller.close();
        return;
      }
      const current = plan[index];
      if (current === undefined) {
        controller.error(problem("not_found"));
        return;
      }
      const currentShare = await activePublicShare(env, shareId, Date.now());
      const currentSession = await uploadSession(
        env,
        currentShare.vault_id,
        currentShare.upload_id,
      );
      if (currentSession === null) {
        controller.error(problem("not_found"));
        return;
      }
      const receipt = await env.DB.prepare(
        "SELECT r2_key, byte_size, digest FROM upload_chunks WHERE upload_id = ? AND chunk_index = ?",
      )
        .bind(currentSession.upload_id, current.index)
        .first<{ byte_size: number; digest: string; r2_key: string }>();
      if (
        receipt === null ||
        receipt.byte_size !== current.size ||
        receipt.digest !== current.sha256
      ) {
        controller.error(problem("not_found"));
        return;
      }
      const stored = await env.CIPHERTEXT.get(receipt.r2_key);
      if (stored === null || !matchingR2Object(stored, current)) {
        controller.error(problem("not_found"));
        return;
      }
      if (!prefixed) {
        const prefix = new Uint8Array(4);
        new DataView(prefix.buffer).setUint32(0, current.size, false);
        controller.enqueue(prefix);
        prefixed = true;
      }
      reader = stored.body.getReader();
    },
    async cancel(): Promise<void> {
      if (reader !== null) await reader.cancel();
    },
  });
}

async function downloadShare(
  request: Request,
  env: Env,
  route: Route,
  now: number,
): Promise<Response> {
  methodIs(request, "GET");
  const shareId = route.ids.shareId;
  if (shareId === undefined) throw problem("not_found");
  const share = await activePublicShare(env, shareId, now);
  const session = await uploadSession(env, share.vault_id, share.upload_id);
  if (session === null) throw problem("not_found");
  return bytesResponse(
    encryptedShareFile(env, shareId, share, session),
    200,
    {
      "Content-Disposition": 'attachment; filename="sona-share.sona"',
      "Content-Type": "application/vnd.sona.encrypted-share",
    },
    now,
  );
}

async function revokeShare(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  context: ExecutionContext,
  now: number,
): Promise<Response> {
  methodIs(request, "DELETE");
  requireContentType(request, "application/json");
  parseVersionOnly(body);
  const auth = await authenticate(request, env, route, body, true, now);
  const shareId = route.ids.shareId;
  if (shareId === undefined) throw problem("not_found");
  const target = `share:${shareId}`;
  const replay = await existingMutationResponse(
    env,
    auth,
    "revoke_share",
    target,
  );
  if (replay !== null) return replay;
  const share = await shareRow(env, auth.vaultId, shareId);
  if (
    share === null ||
    (share.state !== "pending" && share.state !== "active")
  ) {
    throw problem("not_found");
  }
  const session = await uploadSession(env, auth.vaultId, share.upload_id);
  if (session === null) throw problem("not_found");
  const response = { share_id: shareId, state: "revoked" };
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const job = newMaintenanceJob({
    vaultId: auth.vaultId,
    targetId: shareId,
    jobClass: "cleanup_share",
    payload: {},
    now,
  });
  await env.DB.batch([
    env.DB.prepare(
      "UPDATE shares SET state = 'revoked', revoked_at = ? WHERE vault_id = ? AND share_id = ? AND state IN ('pending', 'active')",
    ).bind(now, auth.vaultId, shareId),
    env.DB.prepare(
      "UPDATE upload_sessions SET state = 'cancelled', updated_at = ? WHERE upload_id = ? AND state = 'active' AND EXISTS (SELECT 1 FROM shares WHERE vault_id = ? AND share_id = ? AND state = 'revoked')",
    ).bind(now, session.upload_id, auth.vaultId, shareId),
    env.DB.prepare(
      "UPDATE vaults SET reserved_bytes = reserved_bytes - ?, used_bytes = used_bytes + ? WHERE vault_id = ? AND changes() = 1",
    ).bind(session.reserved_bytes, session.reserved_bytes, auth.vaultId),
    env.DB.prepare(
      "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) SELECT ?, ?, ?, 'revoke_share', ?, ?, 200, ?, ? WHERE EXISTS (SELECT 1 FROM shares WHERE vault_id = ? AND share_id = ? AND state = 'revoked')",
    ).bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      target,
      auth.bodyDigest,
      JSON.stringify(response),
      now,
      auth.vaultId,
      shareId,
    ),
    env.DB.prepare(
      "INSERT INTO maintenance_jobs (job_id, vault_id, job_class, target_id, payload_json, state, next_attempt_at, created_at, updated_at) SELECT ?, ?, 'cleanup_share', ?, '{}', 'queued', ?, ?, ? WHERE EXISTS (SELECT 1 FROM shares WHERE vault_id = ? AND share_id = ? AND state = 'revoked')",
    ).bind(
      job.jobId,
      auth.vaultId,
      shareId,
      now,
      now,
      now,
      auth.vaultId,
      shareId,
    ),
  ]);
  scheduleMaintenance(env, context, job.jobId);
  return jsonResponse(response, 200, now);
}

function parseLiveRoom(body: Uint8Array) {
  const value = parseJsonBody(body);
  assertExactKeys(value, ["version", "roomId", "hostVerifier", "joinVerifier"]);
  if (
    requiredInteger(value, "version", PROTOCOL_VERSION, PROTOCOL_VERSION) !==
    PROTOCOL_VERSION
  ) {
    throw problem("unsupported_version");
  }
  const hostVerifier = digest(requiredString(value, "hostVerifier", 43));
  const joinVerifier = digest(requiredString(value, "joinVerifier", 43));
  if (hostVerifier === joinVerifier) throw problem("invalid_request");
  return {
    hostVerifier,
    joinVerifier,
    roomId: requiredOpaqueId(value, "roomId"),
  };
}

function pageOrigin(request: Request, env: Env): string {
  return new URL(env.PAGE_ORIGIN ?? request.url).origin;
}

async function createLiveRoom(
  request: Request,
  env: Env,
  route: Route,
  body: Uint8Array,
  now: number,
): Promise<Response> {
  requireContentType(request, "application/json");
  const room = parseLiveRoom(body);
  const auth = await authenticate(request, env, route, body, true, now);
  const target = `live:${room.roomId}`;
  const replay = await existingMutationResponse(
    env,
    auth,
    "create_live_room",
    target,
  );
  if (replay !== null) return replay;
  const idempotencyKey = auth.idempotencyKey;
  if (idempotencyKey === undefined) throw problem("invalid_request");
  const expiresAt = await env.LIVE_ROOMS.getByName(room.roomId).create({
    expiresAt: now + LIVE_ROOM_TTL_MS,
    hostVerifier: room.hostVerifier,
    joinVerifier: room.joinVerifier,
    vaultId: auth.vaultId,
  });
  if (expiresAt === null) throw problem("idempotency_conflict");
  const response = {
    room_id: room.roomId,
    page_url: `${pageOrigin(request, env)}/l/${room.roomId}`,
    expires_at: expiresAt,
  };
  const stored = await env.DB.prepare(
    "INSERT INTO idempotency_results (vault_id, device_id, idempotency_key, operation, target, body_digest, status, response_json, created_at) VALUES (?, ?, ?, 'create_live_room', ?, ?, 200, ?, ?) ON CONFLICT DO NOTHING",
  )
    .bind(
      auth.vaultId,
      auth.deviceId,
      idempotencyKey,
      target,
      auth.bodyDigest,
      JSON.stringify(response),
      now,
    )
    .run();
  if (changeCount(stored) === 1) return jsonResponse(response, 200, now);
  const duplicate = await existingMutationResponse(
    env,
    auth,
    "create_live_room",
    target,
  );
  if (duplicate !== null) return duplicate;
  throw problem("invalid_request");
}

async function connectLiveRoom(
  request: Request,
  env: Env,
  route: Route,
): Promise<Response> {
  const roomId = route.ids.roomId;
  if (roomId === undefined) throw problem("not_found");
  const url = new URL(request.url);
  canonicalQuery(url, route.queryKeys);
  if (request.headers.get("upgrade")?.toLowerCase() !== "websocket")
    throw problem("invalid_request");
  const token = base64UrlDecode(url.searchParams.get("t") ?? "");
  const verifier = token?.length === 32 ? await sha256Base64Url(token) : "";
  const response = await env.LIVE_ROOMS.getByName(roomId).fetch(url, {
    headers: { Upgrade: "websocket", [LIVE_VERIFIER_HEADER]: verifier },
  });
  if (response.status === 101) return response;
  if (response.status === 403)
    throw new ApiProblem("unauthorized", { status: 403 });
  if (response.status === 429) throw problem("rate_limited");
  if (response.status === 404) return goneLiveRoom();
  throw problem("not_found");
}

// A page sees a refused upgrade only as a dropped connection, so it learns
// its room is gone from a socket that closes as it opens.
function goneLiveRoom(): Response {
  const pair = new WebSocketPair();
  pair[1].accept();
  pair[1].close(LIVE_ROOM_GONE, "The room is gone");
  return new Response(null, { status: 101, webSocket: pair[0] });
}

function publicRouteQuery(request: Request, route: Route): void {
  canonicalQuery(new URL(request.url), route.queryKeys);
}

function noRequestBody(request: Request): void {
  const length = request.headers.get("content-length");
  if (length !== null && length !== "0") throw problem("invalid_request");
  if (request.headers.get("content-encoding") !== null)
    throw problem("invalid_request");
}

async function serveAsset(
  request: Request,
  env: Env,
  viewer: boolean,
  now: number,
  connectSources = "'self'",
): Promise<Response> {
  const assetRequest = viewer
    ? new Request(new URL("/index.html", request.url), { method: "GET" })
    : request;
  const asset = await env.ASSETS.fetch(assetRequest);
  const headers = new Headers(asset.headers);
  headers.set("Cache-Control", "no-store");
  headers.set("Date", new Date(now).toUTCString());
  headers.set(
    "Content-Security-Policy",
    `default-src 'none'; base-uri 'none'; connect-src ${connectSources}; font-src 'none'; form-action 'none'; frame-ancestors 'none'; frame-src 'none'; img-src 'self' data:; manifest-src 'none'; media-src 'self' blob:; object-src 'none'; script-src 'self'; style-src 'self'; worker-src 'none'`,
  );
  headers.set(
    "Permissions-Policy",
    "camera=(), geolocation=(), microphone=(), payment=(), usb=()",
  );
  headers.set("Referrer-Policy", "no-referrer");
  headers.set("X-Content-Type-Options", "nosniff");
  return new Response(asset.body, { status: asset.status, headers });
}

async function dispatch(
  request: Request,
  env: Env,
  context: ExecutionContext,
  route: Route,
  now: number,
): Promise<Response> {
  if (route.name === "health") {
    methodIs(request, "GET");
    publicRouteQuery(request, route);
    return jsonResponse({ status: "ok" }, 200, now);
  }
  if (route.name === "assets") return serveAsset(request, env, false, now);
  if (route.name === "share_page") {
    methodIs(request, "GET");
    publicRouteQuery(request, route);
    return serveAsset(request, env, true, now);
  }
  if (route.name === "live_page") {
    methodIs(request, "GET");
    publicRouteQuery(request, route);
    const socketOrigin = pageOrigin(request, env).replace(/^http/u, "ws");
    return serveAsset(request, env, true, now, `'self' ${socketOrigin}`);
  }
  if (route.name === "live_create") {
    methodIs(request, "POST");
    const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
    return createLiveRoom(request, env, route, body, now);
  }
  if (route.name === "live_socket") {
    methodIs(request, "GET");
    noRequestBody(request);
    return connectLiveRoom(request, env, route);
  }
  if (route.name === "share_manifest") {
    publicRouteQuery(request, route);
    return shareManifest(request, env, route, now);
  }
  if (route.name === "share_chunk") {
    publicRouteQuery(request, route);
    return shareChunk(request, env, route, now);
  }
  if (route.name === "share_download") {
    publicRouteQuery(request, route);
    return downloadShare(request, env, route, now);
  }
  if (route.name === "bootstrap") {
    methodIs(request, "POST");
    publicRouteQuery(request, route);
    const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
    return bootstrapDevice(request, env, body, now);
  }
  if (route.name === "devices_pair") {
    methodIs(request, "POST");
    const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
    return pairDevice(request, env, route, body, now);
  }
  if (route.name === "devices") return listDevices(request, env, route, now);
  if (route.name === "devices_self")
    return selfDevice(request, env, route, now);
  if (route.name === "device_delete") {
    const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
    return revokeDevice(request, env, route, body, now);
  }
  if (route.name === "capabilities") {
    methodIs(request, "GET");
    noRequestBody(request);
    await authenticate(request, env, route, noBody, false, now);
    return jsonResponse(capabilities(), 200, now);
  }
  if (route.name === "upload_create") {
    methodIs(request, "POST");
    const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
    return createUpload(request, env, route, body, "object", context, now);
  }
  if (route.name === "shares") {
    methodIs(request, "POST");
    const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
    return createUpload(request, env, route, body, "share", context, now);
  }
  if (route.name === "upload_status") {
    if (request.method === "GET") {
      noRequestBody(request);
      return uploadStatus(request, env, route, now);
    }
    if (request.method === "DELETE") {
      const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
      return cancelUpload(request, env, route, body, context, now);
    }
    throw problem("not_found");
  }
  if (route.name === "upload_chunk") {
    const body = await readLimitedBody(request, MAX_CHUNK_BYTES, true);
    return putUploadChunk(request, env, route, body, now);
  }
  if (route.name === "upload_commit") {
    const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
    return commitUpload(request, env, route, body, now);
  }
  if (route.name === "changes") {
    noRequestBody(request);
    return changesPage(request, env, route, now);
  }
  if (route.name === "snapshot") {
    noRequestBody(request);
    return snapshotPage(request, env, route, now);
  }
  if (route.name === "object_manifest") {
    noRequestBody(request);
    return revisionManifest(request, env, route, now);
  }
  if (route.name === "object_chunk") {
    noRequestBody(request);
    return revisionChunk(request, env, route, now);
  }
  if (route.name === "object_delete") {
    const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
    return deleteObject(request, env, route, body, context, now);
  }
  if (route.name === "share_delete") {
    const body = await readLimitedBody(request, MAX_JSON_BYTES, true);
    return revokeShare(request, env, route, body, context, now);
  }
  throw problem("not_found");
}

const worker: ExportedHandler<Env, unknown, { job_id: string }> = {
  async fetch(request, env, context): Promise<Response> {
    const now = Date.now();
    const requestContext: RequestContext = {
      requestId: randomId(12),
      route: "unknown",
      startedAt: now,
    };
    try {
      const route = routeFor(new URL(request.url).pathname);
      requestContext.route = route.name;
      const response = await dispatch(request, env, context, route, now);
      logRequest(env, requestContext, response.status);
      return response;
    } catch (error) {
      const typed =
        error instanceof ApiProblem ? error : dependencyProblem("d1");
      const response = errorResponse(typed, requestContext.requestId, now);
      if (typed.dependency === undefined) {
        logRequest(env, requestContext, response.status, { error: typed.code });
      } else {
        logRequest(env, requestContext, response.status, {
          error: typed.code,
          dependency: typed.dependency,
        });
      }
      return response;
    }
  },
  scheduled(_controller, env, context): void {
    context.waitUntil(runMaintenance(env));
  },
  async queue(batch, env): Promise<void> {
    await forEachBounded(
      batch.messages,
      MAX_CONCURRENT_QUEUE_JOBS,
      async (message) => {
        const body = message.body;
        if (isJsonRecord(body)) {
          const jobId = body.job_id;
          if (isJsonString(jobId) && isOpaqueId(jobId)) {
            await maintenanceQueueMessage(env, { job_id: jobId });
          }
        }
        message.ack();
      },
    );
  },
};

export default worker;
export { LiveRoom } from "./live";
