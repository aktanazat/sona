import type {
  AllowedMeetingAction,
  CaptureCompleteness,
  MeetingCommandError,
  MeetingListFilter,
  MeetingPhase,
  MeetingSessionSnapshot,
  MeetingProvider,
  MeetingReasonCode,
  ProcessingStatus,
  SourceAvailability,
  SourceKind,
} from "@/bindings";

export const MEETING_SOURCES: SourceKind[] = ["microphone", "system_audio"];

const MEETING_DATE_FORMATTER = new Intl.DateTimeFormat(undefined, {
  dateStyle: "medium",
  timeStyle: "short",
});

/** "0:00" | "9:41" | "1:04:07" — a clock reading, seconds in, no unit words. */
export const formatClock = (seconds: number) => {
  const total = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const remainder = total % 60;

  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`
    : `${minutes}:${String(remainder).padStart(2, "0")}`;
};

export const formatMeetingOffset = (offsetNs: number | null | undefined) => {
  if (offsetNs === null || offsetNs === undefined) {
    return "—";
  }

  return formatClock(offsetNs / 1_000_000_000);
};

export const formatMeetingDate = (timestamp: number) =>
  MEETING_DATE_FORMATTER.format(timestamp);

export const meetingProviderKey = (provider: MeetingProvider) =>
  `meetings.providers.${provider}`;

export const meetingPhaseKey = (phase: MeetingPhase) =>
  `meetings.phases.${phase}`;

export const sourceKey = (source: SourceKind) => `meetings.sources.${source}`;

export const sourceAvailabilityKey = (availability: SourceAvailability) =>
  `meetings.availability.${availability}`;

export const captureCompletenessKey = (completeness: CaptureCompleteness) =>
  `meetings.completeness.${completeness}`;

export const processingStatusKey = (status: ProcessingStatus) => {
  if (status.kind === "failed") {
    return `meetings.processing.failed.${status.reason}`;
  }

  return `meetings.processing.${status.kind}`;
};

const MEETING_ERROR_KEYS = {
  consent_required: "meetings.errors.consent_required",
  consent_stale: "meetings.errors.consent_stale",
  invalid_transition: "meetings.errors.invalid_transition",
  stale_revision: "meetings.errors.stale_revision",
  capture_lease_busy: "meetings.errors.capture_lease_busy",
  no_source_started: "meetings.errors.no_source_started",
  source_unavailable: "meetings.errors.source_unavailable",
  storage_unavailable: "meetings.errors.storage_unavailable",
  recovery_required: "meetings.errors.recovery_required",
  deletion_in_progress: "meetings.errors.deletion_in_progress",
  not_found: "meetings.errors.not_found",
  invalid_request: "meetings.errors.invalid_request",
  export_cancelled: "meetings.errors.export_cancelled",
  export_failed: "meetings.errors.export_failed",
  local_model_unavailable: "meetings.errors.local_model_unavailable",
  local_evidence_unavailable: "meetings.review.rememberVoiceFailed",
  insufficient_enrollment_evidence: "meetings.review.rememberVoiceFailed",
  profile_model_incompatible: "meetings.review.rememberVoiceFailed",
  profile_merge_resolution_required:
    "meetings.errors.profile_merge_resolution_required",
  remote_unavailable: "meetings.errors.remote_unavailable",
  engine_failure: "meetings.processing.failed.engine_failure",
  import_unreadable: "meetings.errors.import_unreadable",
} as const satisfies Record<MeetingCommandError, string>;

export const meetingErrorKey = (error: MeetingCommandError) =>
  MEETING_ERROR_KEYS[error];

const MEETING_REASON_KEYS = {
  consent_missing: "meetings.reasons.consent_missing",
  consent_stale: "meetings.reasons.consent_stale",
  stale_revision: "meetings.reasons.stale_revision",
  capture_lease_busy: "meetings.reasons.capture_lease_busy",
  source_unavailable: "meetings.reasons.source_unavailable",
  source_start_failed: "meetings.reasons.source_start_failed",
  source_gap: "meetings.reasons.source_gap",
  storage_unavailable: "meetings.reasons.storage_unavailable",
  storage_failure: "meetings.reasons.storage_failure",
  local_model_unavailable: "meetings.reasons.local_model_unavailable",
  recovery_required: "meetings.reasons.recovery_required",
  deleted: "meetings.reasons.deleted",
  invalid_transition: "meetings.reasons.invalid_transition",
  duplicate_operation: "meetings.reasons.duplicate_operation",
} as const satisfies Record<MeetingReasonCode, string>;

export const meetingReasonKey = (reason: MeetingReasonCode) =>
  MEETING_REASON_KEYS[reason];

export const isActiveMeetingPhase = (phase: MeetingPhase) =>
  phase === "capturing_recording" ||
  phase === "capturing_pausing" ||
  phase === "capturing_paused" ||
  phase === "capturing_resuming" ||
  phase === "starting" ||
  phase === "stopping";

export const isPreflightMeetingPhase = (phase: MeetingPhase) =>
  phase === "preflight";

export const preflightAllowsAction = (
  session: Pick<MeetingSessionSnapshot, "phase" | "allowed_actions">,
  action: AllowedMeetingAction,
) =>
  isPreflightMeetingPhase(session.phase) &&
  session.allowed_actions.includes(action);

/* No filter at all: the whole list, newest first, which after round 7 is the
 * only shape a filter takes besides a title the reader typed. The status and
 * window pickers are gone, so `status` and `window` stay "any" for the life of
 * the screen and the store keeps answering the query it always did. */
export const NO_MEETING_FILTER: MeetingListFilter = {
  status: "any",
  window: "any",
  title_query: "",
};
