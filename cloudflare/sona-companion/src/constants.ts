export const PROTOCOL_VERSION = 1;
export const CRYPTO_VERSION = 1;

export const MAX_CLOCK_SKEW_MS = 5 * 60 * 1000;
export const NONCE_RETENTION_MS = 10 * 60 * 1000;
export const UPLOAD_TTL_MS = 24 * 60 * 60 * 1000;
export const JOB_LEASE_MS = 60 * 1000;
export const JOB_RETRY_MS = 5 * 60 * 1000;
export const CHANGE_RETENTION_MS = 30 * 24 * 60 * 60 * 1000;
export const LIVE_ROOM_TTL_MS = 7 * 24 * 60 * 60 * 1000;
export const LIVE_ROOM_GONE = 4404;

export const MAX_REMOTE_BYTES = 8 * 1024 * 1024 * 1024;
export const MAX_CHUNK_BYTES = 4 * 1024 * 1024;
export const MAX_CHUNKS_PER_UPLOAD = 4096;
export const MAX_ACTIVE_UPLOADS = 16;
export const MAX_ACTIVE_SHARES = 32;
export const MAX_SHARE_BYTES = 256 * 1024 * 1024;
export const MAX_SHARE_TTL_MS = 30 * 24 * 60 * 60 * 1000;
export const MAX_MANIFEST_BYTES = 512 * 1024;
export const MAX_JSON_BYTES = 1024 * 1024;
export const MAX_CHANGE_PAGE = 100;
export const MAX_SNAPSHOT_PAGE = 100;
export const MAINTENANCE_PAGE_SIZE = 8;
export const MAX_LIVE_GUESTS = 50;
export const MAX_LIVE_HOST_FRAME_BYTES = 1000 * 1000;
export const MAX_LIVE_GUEST_FRAME_BYTES = 16 * 1024;

export type ApiErrorCode =
  | "invalid_request"
  | "unauthorized"
  | "revoked_device"
  | "clock_skew"
  | "replay"
  | "idempotency_conflict"
  | "not_found"
  | "stale_revision"
  | "upload_incomplete"
  | "chunk_conflict"
  | "integrity_failed"
  | "unsupported_version"
  | "quota_exceeded"
  | "rate_limited"
  | "dependency_unavailable"
  | "cursor_expired";

export type TombstoneReason = "user_request" | "retention";

export type ShareState =
  | "pending"
  | "active"
  | "revoked"
  | "expired"
  | "purged";

export type UploadState =
  | "active"
  | "committed"
  | "cancelled"
  | "expired"
  | "purged";

export type MaintenanceClass =
  | "reconcile_upload"
  | "cleanup_upload"
  | "cleanup_object"
  | "cleanup_share"
  | "compact_changes";
