import type {
  DegradedStartPolicy,
  ManualNote,
  MeetingConsentInput,
  MeetingExportFormat,
  MeetingHistorySummary,
  MeetingListFilter,
  MeetingNavigationPayload,
  MeetingOrigin,
  MeetingReviewSnapshot,
  MeetingSuggestion,
  MeetingSuggestionId,
  OperationReceipt,
  ProcessingDestination,
  SourceKind,
  SpeakerId,
} from "@/bindings";
import type { MeetingPreviewFacts } from "./MeetingPreviewCard";
import type { CalendarEventSummary } from "./detectionStore";

export interface MeetingsSettingsProps {
  invalidation?: number;
  navigationRequest?: MeetingNavigationPayload | null;
  startRequest?: number;
  /**
   * The shell's Settings route. Retention and generated-note recovery both
   * state a setting where it matters, then send the reader to change it.
   */
  onOpenSettings: () => void;
}

/** Everything one press of Start needs. There is no setup screen: these are
 * the defaults the start block shows inline and can flip in place. */
export interface MeetingStartOptions {
  title: string;
  origin: MeetingOrigin;
  suggestionId: MeetingSuggestionId | null;
  calendarEventKey: string | null;
  sources: SourceKind[];
  degradedStartPolicy: DegradedStartPolicy;
  destination: ProcessingDestination;
  /** What the operator was looking at when they pressed Start, so the
   * preflight can show the same meeting rather than a bare title. `null` for
   * a start with no preview behind it, which is the manual press. */
  preview: MeetingPreviewFacts | null;
}

export type MeetingScreen =
  | { kind: "home" }
  /** A session exists but capture has not begun: the start attempt hit an
   * unavailable source, or a preflight session was opened from elsewhere. */
  | { kind: "gate"; sessionId: string; options: MeetingStartOptions }
  | { kind: "session"; sessionId: string };

export type MeetingPendingAction =
  | "start"
  | "preflight_cancel"
  | "preflight_refresh"
  | "pause"
  | "resume"
  | "stop"
  | "discard"
  | "finalize_partial"
  | "title_set"
  | "speaker_rename"
  | "speaker_merge"
  | "segment_edit"
  | "note_create"
  | "note_update"
  | "note_delete"
  | "artifacts_regenerate"
  | "remote_cancel"
  | "delete"
  | "export_ledger"
  | `export_${MeetingExportFormat}`;

export interface MeetingsHomeScreenModel {
  suggestions: MeetingSuggestion[];
  recovery: MeetingHistorySummary[];
  meetings: MeetingHistorySummary[];
  loading: boolean;
  paging: boolean;
  hasMore: boolean;
  page: number;
  filter: MeetingListFilter;
  error: string | null;
  sources: SourceKind[];
  starting: boolean;
  /** The shell asked for a recording, so the page moves focus to Record. */
  focusStart: boolean;
}

export interface MeetingsHomeScreenActions {
  onStart: () => void;
  onImport: () => void;
  onStartSuggestion: (suggestion: MeetingSuggestion) => void;
  onStartEvent: (event: CalendarEventSummary) => void;
  onOpenMeeting: (sessionId: string) => void;
  onFinalizeRecovery: (sessionId: string) => void;
  onDiscardRecovery: (sessionId: string) => void;
  onFilterChange: (filter: MeetingListFilter) => void;
  onNextPage: () => void;
  onPreviousPage: () => void;
  onExportMeeting: (sessionId: string, format: MeetingExportFormat) => void;
  onExportLedger: (sessionId: string) => void;
  onDeleteMeeting: (sessionId: string) => void;
  onRetry: () => void;
}

export interface MeetingLoadingScreenModel {
  kind: "loading";
  label: string;
}

export interface MeetingGateScreenModel {
  kind: "gate";
  snapshot: MeetingReviewSnapshot;
  options: MeetingStartOptions;
  refreshing: boolean;
  starting: boolean;
}

export interface MeetingGateScreenActions {
  onRefresh: () => void;
  onCancel: () => void;
  onStart: (consent: MeetingConsentInput) => void;
}

export interface MeetingLiveScreenModel {
  snapshot: MeetingReviewSnapshot;
  pendingAction: MeetingPendingAction | null;
}

export interface MeetingLiveScreenActions {
  onPause: () => void;
  onResume: () => void;
  onStop: () => void;
  onDiscard: () => void;
  onCreateNote: (body: string) => void;
}

export interface MeetingReviewScreenModel {
  snapshot: MeetingReviewSnapshot;
  lastReceipt: OperationReceipt | null;
  pendingAction: MeetingPendingAction | null;
}

export interface MeetingReviewScreenActions {
  onBack: () => void;
  onTitleSet: (title: string) => void;
  onSpeakerRename: (speakerId: SpeakerId, displayName: string) => void;
  onSpeakerMerge: (
    sourceSpeakerId: SpeakerId,
    targetSpeakerId: SpeakerId,
  ) => void;
  onSegmentEdit: (
    segmentId: string,
    replacementText: string,
    removed: boolean,
  ) => void;
  onNoteCreate: (body: string) => void;
  onNoteUpdate: (note: ManualNote, body: string) => void;
  onNoteDelete: (note: ManualNote) => void;
  onRegenerate: () => void;
  onExport: (format: MeetingExportFormat) => void;
  onRemoteCancel: () => void;
  onDelete: () => void;
  onRefresh: () => Promise<void>;
}

/** Three public members replace the old flattened 41-member controller.
 * Each branch carries only the model and actions for the screen it names. */
export type MeetingsController =
  | {
      screen: "home";
      model: MeetingsHomeScreenModel;
      actions: MeetingsHomeScreenActions;
    }
  | {
      screen: "loading";
      model: MeetingLoadingScreenModel;
      actions: null;
    }
  | {
      screen: "gate";
      model: MeetingGateScreenModel;
      actions: MeetingGateScreenActions;
    }
  | {
      screen: "live";
      model: MeetingLiveScreenModel;
      actions: MeetingLiveScreenActions;
    }
  | {
      screen: "review";
      model: MeetingReviewScreenModel;
      actions: MeetingReviewScreenActions;
    };
