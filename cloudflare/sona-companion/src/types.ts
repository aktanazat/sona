import type {
  ApiErrorCode,
  MaintenanceClass,
  ShareState,
  TombstoneReason,
  UploadState,
} from "./constants";
import type { LiveRoom } from "./live";

export interface Env {
  ASSETS: Fetcher;
  AUDIENCE: string;
  BOOTSTRAP_SECRET: string;
  CIPHERTEXT: R2Bucket;
  DB: D1Database;
  LIVE_ROOMS: DurableObjectNamespace<LiveRoom>;
  MAINTENANCE: Queue<MaintenanceMessage>;
  PAGE_ORIGIN?: string;
  WORKER_VERSION?: string;
}

export interface MaintenanceMessage {
  job_id: string;
}

export interface ApiErrorBody {
  code: ApiErrorCode;
  request_id: string;
  retryable: boolean;
}

export interface RequestContext {
  requestId: string;
  route: string;
  startedAt: number;
}

export interface AuthenticatedRequest {
  body: Uint8Array;
  bodyDigest: string;
  deviceId: string;
  idempotencyKey?: string;
  nonce: Uint8Array;
  signature: Uint8Array;
  signingPublicKey: Uint8Array;
  timestamp: number;
  vaultId: string;
}

export interface ActiveDeviceRow {
  device_id: string;
  signing_public_key: ArrayBuffer;
  status: "active" | "revoked";
}

export interface UploadPlanChunk {
  index: number;
  sha256: string;
  size: number;
}

export interface UploadPlan {
  baseRevisionId: string | null;
  chunkCount: number;
  chunks: UploadPlanChunk[];
  cryptoVersion: number;
  manifest: Uint8Array;
  manifestDigest: string;
  objectId?: string;
  revisionId?: string;
  shareExpiresAt?: number;
  shareId?: string;
  totalBytes: number;
  uploadId: string;
  writerSignature: Uint8Array;
}

export interface UploadSessionRow {
  base_revision_id: string | null;
  chunk_count: number;
  chunk_plan: string;
  cleanup_accounted: number;
  committed_sequence: number | null;
  creator_device_id: string;
  crypto_version: number;
  expires_at: number;
  kind: "object" | "share";
  manifest: ArrayBuffer;
  manifest_digest: string;
  object_id: string | null;
  reserved_bytes: number;
  revision_id: string | null;
  share_id: string | null;
  state: UploadState;
  total_bytes: number;
  upload_id: string;
  vault_id: string;
  writer_signature: ArrayBuffer;
}

export interface ObjectHeadRow {
  head_revision_id: string;
  head_sequence: number;
  object_id: string;
  tombstone: number;
}

export interface RevisionRow {
  chunk_count: number;
  crypto_version: number;
  manifest: ArrayBuffer;
  manifest_digest: string;
  manifest_size: number;
  object_id: string;
  parent_revision_id: string | null;
  revision_id: string;
  tombstone: number;
  tombstone_reason: TombstoneReason | null;
  total_bytes: number;
  writer_device_id: string;
  writer_signature: ArrayBuffer;
}

export interface ShareRow {
  chunk_count: number;
  crypto_version: number;
  expires_at: number;
  manifest_digest: string;
  share_id: string;
  state: ShareState;
  total_bytes: number;
  upload_id: string;
  writer_signature: ArrayBuffer;
}

export interface MaintenanceJobRow {
  attempt_count: number;
  job_class: MaintenanceClass;
  job_id: string;
  lease_token: string;
  payload_json: string;
  progress: number;
  state: "queued" | "running" | "done";
  target_id: string;
  vault_id: string;
}

export interface IdempotencyRow {
  body_digest: string;
  operation: string;
  response_json: string;
  status: number;
  target: string;
}

export interface LogEvent {
  dependency?: "d1" | "r2" | "queue";
  error?: ApiErrorCode;
  latency_bucket: "lt_10ms" | "lt_100ms" | "lt_1s" | "gte_1s";
  maintenance_class?: MaintenanceClass;
  request_id: string;
  route: string;
  status: number;
  worker_version: string;
}
