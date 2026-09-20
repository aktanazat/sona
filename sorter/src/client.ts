import { z } from "zod";
import { canonicalRequestBytes } from "../../cloudflare/sona-companion/src/crypto";
import {
  base64UrlDecode,
  base64UrlEncode,
  decodeUtf8,
  sha256Base64Url,
  utf8,
} from "../../cloudflare/sona-companion/src/encoding";
import { signEd25519 } from "./keys";

const AUDIENCE = "sona-companion";
const PAGE_LIMIT = 100;

export interface DeviceIdentity {
  deviceId: string;
  signingSeed: Uint8Array;
}

const ChangeRow = z.object({
  object_id: z.string(),
  revision_id: z.string(),
  sequence: z.number().int(),
  tombstone: z.boolean(),
});
export type ChangeRow = z.infer<typeof ChangeRow>;

const ChangesPage = z.object({
  changes: z.array(ChangeRow),
  has_more: z.boolean(),
  high_water: z.number().int(),
  next_cursor: z.string(),
});
export type ChangesPage = z.infer<typeof ChangesPage>;

const SnapshotPage = z.object({
  after: z.string().nullable(),
  has_more: z.boolean(),
  heads: z.array(ChangeRow),
  high_water: z.string(),
});
export type SnapshotPage = z.infer<typeof SnapshotPage>;

const RevisionEnvelope = z.object({
  chunk_count: z.number().int(),
  crypto_version: z.number().int(),
  manifest_sha256: z.string(),
  object_id: z.string(),
  parent_revision_id: z.string().nullable(),
  revision_id: z.string(),
  total_bytes: z.number().int(),
  writer_device_id: z.string(),
  writer_signature: z.string(),
});
export type RevisionEnvelope = z.infer<typeof RevisionEnvelope>;

const RevisionManifest = z.object({ envelope: RevisionEnvelope, manifest: z.string() });
export type RevisionManifest = z.infer<typeof RevisionManifest>;

const SelfDevice = z.object({
  device_id: z.string(),
  envelope: z.string().nullable(),
  pairing_public_key: z.string(),
  protocol_version: z.number().int().nullable(),
  signing_public_key: z.string(),
  status: z.string(),
});
export type SelfDevice = z.infer<typeof SelfDevice>;

export interface UploadChunkPlan {
  index: number;
  sha256: string;
  size: number;
}

/** The `POST /v1/uploads` body; request keys are camelCase, responses snake_case. */
export interface ObjectUploadPlan {
  baseRevisionId: string | null;
  chunkCount: number;
  chunks: UploadChunkPlan[];
  cryptoVersion: 1;
  manifest: string;
  manifestSha256: string;
  objectId: string;
  revisionId: string;
  totalBytes: number;
  uploadId: string;
  version: 1;
  writerSignature: string;
}

const UploadCreated = z.object({
  accepted_indexes: z.array(z.number().int()),
  state: z.string(),
  upload_id: z.string(),
});
export type UploadCreated = z.infer<typeof UploadCreated>;

const ChunkAccepted = z.object({
  accepted: z.boolean(),
  index: z.number().int(),
  upload_id: z.string(),
});
export type ChunkAccepted = z.infer<typeof ChunkAccepted>;

const UploadCommitted = z.object({
  change_sequence: z.number().int(),
  revision_id: z.string(),
  state: z.string(),
  upload_id: z.string(),
});
export type UploadCommitted = z.infer<typeof UploadCommitted>;

/** What a non-2xx reply may carry; a body that is not JSON reads as `{}`. */
const Problem = z.object({ code: z.string().catch("unknown"), retryable: z.boolean().catch(false) });

/** A `high_water` token's payload: the sequence it names. */
const HighWater = z.object({ v: z.literal(1), w: z.number().int().safe() });

/** A non-2xx Worker reply, carrying the body's error code. */
export class ApiError extends Error {
  readonly code: string;
  readonly retryable: boolean;
  readonly status: number;

  constructor(status: number, code: string, retryable: boolean) {
    super(`${status} ${code}`);
    this.code = code;
    this.retryable = retryable;
    this.status = status;
  }
}

interface Mutation {
  body: Uint8Array;
  contentType: string;
  extraHeaders?: Record<string, string>;
  idempotencyKey: string;
}

/** The change cursor after `sequence`, in the Worker's own `c.` encoding. */
export function changeCursorAfter(sequence: number): string {
  return `c.${base64UrlEncode(utf8(JSON.stringify({ v: 1, a: sequence })))}`;
}

/** The sequence a snapshot `high_water` token names. */
export function snapshotHighWater(token: string): number {
  const bytes = token.startsWith("h.") ? base64UrlDecode(token.slice(2)) : null;
  if (bytes === null) throw new Error("invalid snapshot high water");
  return HighWater.parse(JSON.parse(decodeUtf8(bytes))).w;
}

function randomNonce(): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(16));
}

/**
 * Signed HTTP to one Worker as one device.
 *
 * `syncClock` reads the Worker's `Date` header so timestamps land inside its
 * five-minute skew window even on a VPS with a drifting clock.
 */
export class CompanionClient {
  private readonly base: URL;
  private clockOffsetMs = 0;

  constructor(
    endpoint: string,
    private readonly identity: DeviceIdentity,
    private readonly vaultId: string,
  ) {
    this.base = new URL(endpoint);
    if (this.base.protocol !== "https:" && this.base.hostname !== "localhost")
      throw new Error("endpoint must be https");
  }

  nowUtcMs(): number {
    return Date.now() + this.clockOffsetMs;
  }

  async syncClock(): Promise<void> {
    const response = await fetch(new URL("/healthz", this.base));
    const date = Date.parse(response.headers.get("date") ?? "");
    if (!Number.isFinite(date)) throw new Error("healthz carried no date");
    this.clockOffsetMs = date - Date.now();
  }

  selfDevice(): Promise<SelfDevice> {
    return this.json(SelfDevice, "GET", "/v1/devices/self");
  }

  changes(cursor: string | null): Promise<ChangesPage> {
    const query: [string, string][] = [["limit", String(PAGE_LIMIT)]];
    if (cursor !== null) query.push(["cursor", cursor]);
    return this.json(ChangesPage, "GET", "/v1/changes", query);
  }

  snapshot(highWater: string | null, after: string | null): Promise<SnapshotPage> {
    const query: [string, string][] = [["limit", String(PAGE_LIMIT)]];
    if (highWater !== null) query.push(["highWater", highWater]);
    if (after !== null) query.push(["after", after]);
    return this.json(SnapshotPage, "GET", "/v1/snapshot", query);
  }

  manifest(objectId: string, revisionId: string): Promise<RevisionManifest> {
    return this.json(
      RevisionManifest,
      "GET",
      `/v1/objects/${objectId}/revisions/${revisionId}/manifest`,
    );
  }

  async chunk(objectId: string, revisionId: string, index: number): Promise<Uint8Array> {
    const response = await this.send(
      "GET",
      `/v1/objects/${objectId}/revisions/${revisionId}/chunks/${index}`,
      [],
      null,
    );
    return new Uint8Array(await response.arrayBuffer());
  }

  createUpload(plan: ObjectUploadPlan, idempotencyKey: string): Promise<UploadCreated> {
    return this.json(UploadCreated, "POST", "/v1/uploads", [], {
      body: utf8(JSON.stringify(plan)),
      contentType: "application/json",
      idempotencyKey,
    });
  }

  putChunk(
    uploadId: string,
    index: number,
    bytes: Uint8Array,
    sha256: string,
    idempotencyKey: string,
  ): Promise<ChunkAccepted> {
    return this.json(ChunkAccepted, "PUT", `/v1/uploads/${uploadId}/chunks/${index}`, [], {
      body: bytes,
      contentType: "application/octet-stream",
      extraHeaders: { "x-sona-chunk-sha256": sha256 },
      idempotencyKey,
    });
  }

  commitUpload(uploadId: string, idempotencyKey: string): Promise<UploadCommitted> {
    return this.json(UploadCommitted, "POST", `/v1/uploads/${uploadId}/commit`, [], {
      body: utf8(JSON.stringify({ version: 1 })),
      contentType: "application/json",
      idempotencyKey,
    });
  }

  private async json<Value>(
    reply: z.ZodType<Value>,
    method: string,
    path: string,
    query: [string, string][] = [],
    mutation: Mutation | null = null,
  ): Promise<Value> {
    const response = await this.send(method, path, query, mutation);
    return reply.parse(await response.json());
  }

  private async send(
    method: string,
    path: string,
    query: [string, string][],
    mutation: Mutation | null,
  ): Promise<Response> {
    const url = new URL(path, this.base);
    for (const [key, value] of query) url.searchParams.set(key, value);
    const body = mutation?.body ?? new Uint8Array();
    const contentType = mutation?.contentType ?? "";
    const idempotencyKey = mutation?.idempotencyKey ?? "";
    const nonce = randomNonce();
    const timestamp = this.nowUtcMs();
    const signature = await signEd25519(
      this.identity.signingSeed,
      canonicalRequestBytes({
        audience: AUDIENCE,
        vaultId: this.vaultId,
        deviceId: this.identity.deviceId,
        method,
        path: url.pathname,
        query,
        bodyDigest: await sha256Base64Url(body),
        contentType,
        idempotencyKey,
        timestamp,
        nonce,
      }),
    );
    const headers = new Headers(mutation?.extraHeaders);
    headers.set("x-sona-vault-id", this.vaultId);
    headers.set("x-sona-device-id", this.identity.deviceId);
    headers.set("x-sona-timestamp", String(timestamp));
    headers.set("x-sona-nonce", base64UrlEncode(nonce));
    headers.set("x-sona-signature", base64UrlEncode(signature));
    if (mutation !== null) {
      headers.set("content-type", contentType);
      headers.set("x-sona-idempotency-key", idempotencyKey);
    }
    const response = await fetch(url, {
      method,
      headers,
      body: mutation === null ? null : body,
    });
    if (response.ok) return response;
    const problem = Problem.parse(await response.json().catch(() => ({})));
    throw new ApiError(response.status, problem.code, problem.retryable);
  }
}
