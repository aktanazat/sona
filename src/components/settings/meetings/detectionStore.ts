import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { create } from "zustand";
import type {
  DetectionPromptEvent,
  DetectionPromptKind,
  PersonBriefingRow,
} from "@/bindings";

/* Detection's frontend contract.
 *
 * The commands are invoked by name with camelCase argument keys, the same
 * precedent `read_history_audio_chunk` follows, so this store is the one place
 * that names detection's wire shapes. Every type below mirrors a Rust type in
 * src-tauri/src/meeting/detection.rs, which serializes camelCase.
 *
 * The two prompt shapes are the exception, and re-exported rather than
 * mirrored: `DetectionPromptEvent` is registered with the specta builder in
 * lib.rs, so bindings.ts generates it and the union it carries straight from
 * the Rust definitions. A hand mirror of them drifted once already — serde's
 * container `rename_all` renamed the variants instead of their fields, the
 * union matched no arm, and the pane rendered prompt cards with no title on
 * them. Generated types cannot drift, so these two no longer can. */

export type { DetectionPromptEvent, DetectionPromptKind };

export type CalendarAccess =
  | "not_determined"
  | "authorized"
  | "denied"
  | "unavailable";

export type NotificationAccess =
  | "not_determined"
  | "authorized"
  | "denied"
  | "unavailable";

export type DetectionSuppressReason =
  | "detection_disabled"
  | "sona_holds_input_device"
  | "capture_already_active"
  | "no_qualifying_signal"
  | "attendee_floor_not_met"
  | "unknown_mic_source"
  | "browser_title_unreadable"
  | "browser_title_not_meeting"
  | "app_present_not_in_use"
  | "sona_mic_just_closed";

export interface DetectionSettings {
  enabled: boolean;
  calendarEnabled: boolean;
  anyMicActivity: boolean;
  autoStartOnOpenPane: boolean;
  meetingApps: string[];
  /* Bundle IDs that record without a prompt. Empty until the operator turns a
   * switch on: this is the one setting that turns a notice into a recording. */
  autoRecordApps: string[];
}

/** How one participant answered, when EventKit reports an answer at all. */
export type ParticipationStatus =
  | "unknown"
  | "pending"
  | "accepted"
  | "declined"
  | "tentative";

export interface CalendarAttendee {
  name: string;
  status: ParticipationStatus;
  isSelf: boolean;
}

/* Mirrors machine::CalendarEventSummary. Every field below `endUtcMs` is
 * serde-defaulted on the Rust side and absent whenever the calendar did not
 * supply it, which is what lets the pre-meeting card omit a row instead of
 * rendering an empty one. */
export interface CalendarEventSummary {
  eventKey: string;
  /** EventKit's calendar-item identifier, shared by every occurrence of a
   * recurring event. Always sent, empty for an event that recurs not at all,
   * which is what the series surfaces read as "no series". */
  seriesKey: string;
  title: string;
  /** Includes participants EventKit refused to name, so it can exceed
   * `attendees.length`. */
  attendeeCount: number;
  startUtcMs: number;
  endUtcMs: number;
  attendees: CalendarAttendee[];
  notes: string | null;
  calendarName: string | null;
  url: string | null;
}

export interface DetectionCountdown {
  event: CalendarEventSummary;
  secondsToStart: number;
  briefing: PersonBriefingRow[];
}

export interface AdoptedCall {
  bundleId: string;
  displayName: string;
}

export interface DetectionStatus {
  eventSchemaVersion: number;
  settings: DetectionSettings;
  calendarAccess: CalendarAccess;
  notificationAccess: NotificationAccess;
  inputDeviceActive: boolean;
  sonaHoldsInputDevice: boolean;
  suppressReason: DetectionSuppressReason | null;
  countdown: DetectionCountdown | null;
  adoptedCall: AdoptedCall | null;
  runningMeetingApps: string[];
  inputDeviceReportingSuspect: boolean;
}

/* What an offer is *about*, as opposed to which delivery of it this is.
 *
 * The backend mints a fresh prompt id on every raise
 * (detection.rs `raise`) and re-arms an application's claim every time the
 * input device goes idle (detection.rs `publish_status`), so the same app
 * across three microphone episodes legitimately produces three prompt ids for
 * one subject. Keyed by id alone, all three stacked into three identical
 * cards that nothing ever removed. */
const promptSubject = (entry: DetectionPromptEvent): string => {
  const prompt = entry.prompt;
  switch (prompt.kind) {
    case "CalendarEvent":
      return `event:${prompt.eventKey}`;
    case "AppMeeting":
    case "AppHuddle":
    case "BrowserCall":
    case "AppCall":
      return `app:${prompt.bundleId}`;
    case "UnknownMicSource":
      return "mic";
  }
  /* A kind this build does not know is no evidence that two prompts are the
   * same offer, so key it by id, which never merges two distinct ones. */
  return entry.promptId;
};

interface DetectionStore {
  status: DetectionStatus | null;
  /* Prompts still waiting for an answer, oldest first. One entry per subject:
   * a re-raise supersedes the earlier offer rather than joining it. */
  prompts: DetectionPromptEvent[];
  setStatus: (status: DetectionStatus) => void;
  addPrompt: (prompt: DetectionPromptEvent) => void;
  clearPrompt: (promptId: string) => void;
  refresh: () => Promise<void>;
  answer: (promptId: string, accepted: boolean) => Promise<void>;
  /* True while `detection_settings_set` is in flight.
   *
   * One flag for the whole app, because the surfaces that write these settings
   * are not one component and not even one page: the master switch is an
   * Essentials row, the app picker is the row under it, the calendar switch is
   * on Advanced and the countdown card writes `autoStartOnOpenPane` from the
   * Meetings page. A per-component boolean leaves every one of them believing
   * nothing is being written. */
  savingSettings: boolean;
  /* True after an enable attempt that macOS answered without a dialog and
   * without full access — the decided-permission case a reader cannot see. */
  calendarRefused: boolean;
  patch: (change: Partial<DetectionSettings>) => Promise<void>;
  enableCalendar: (enabled: boolean) => Promise<void>;
  requestCalendarAccess: () => Promise<CalendarAccess>;
  requestNotificationAccess: () => Promise<NotificationAccess>;
}

export const useDetectionStore = create<DetectionStore>()((set, get) => ({
  status: null,
  prompts: [],
  savingSettings: false,
  calendarRefused: false,
  setStatus: (status) =>
    set((state) => ({
      status,
      /* An offer to record a meeting is live only while something holds the
       * microphone. The device going idle is the same signal detection's own
       * auto-stop reads as `input_device_idle` and the same moment the backend
       * re-arms the app's claim, so an unanswered ad-hoc prompt from that
       * episode is a stale offer rather than a pending one.
       *
       * Two kinds outlive the device deliberately, and the backend retracts
       * both by their own boundary. Calendar prompts are raised at T-60s,
       * before anyone has opened a microphone. Call prompts are raised for a
       * FaceTime or Phone call, which on a Bluetooth headset may never raise
       * the input device at all — pruning those here would erase the offer on
       * the tick that made it. Statuses arrive on change only, so this is one
       * prune per episode, and the untouched case keeps the array identity the
       * prompt subscription compares on. */
      prompts: status.inputDeviceActive
        ? state.prompts
        : state.prompts.filter(
            (entry) =>
              entry.prompt.kind === "CalendarEvent" ||
              entry.prompt.kind === "AppCall",
          ),
    })),
  addPrompt: (prompt) =>
    set((state) => {
      const subject = promptSubject(prompt);
      return {
        prompts: [
          ...state.prompts.filter((entry) => promptSubject(entry) !== subject),
          prompt,
        ],
      };
    }),
  clearPrompt: (promptId) =>
    set((state) => ({
      prompts: state.prompts.filter((entry) => entry.promptId !== promptId),
    })),

  /* Both status paths go through `setStatus`, so the rule about which prompts a
   * status leaves standing lives in exactly one place. */
  refresh: async () => {
    const status = await invoke<DetectionStatus>("detection_status_get");
    get().setStatus(status);
  },

  answer: async (promptId, accepted) => {
    /* Clear locally first: the card must stop offering a decision the operator
     * has already made, and the backend drops an unknown prompt id anyway. */
    get().clearPrompt(promptId);
    await invoke("detection_prompt_respond", { promptId, accepted });
  },

  /* Detection's one write.
   *
   * `detection_settings_set` takes the whole struct, deliberately: the backend
   * refuses to represent a half-written state such as
   * calendar-on-while-detection-off. Two overlapping writes are therefore two
   * full overwrites, and whichever lands second reverts every field the first
   * one changed.
   *
   * Two things stop that here. The base is read at call time rather than
   * supplied by the caller, so a row holding a render-old snapshot cannot send
   * the fields it never touched back to what they were before the last write;
   * and a write refuses to start while one is in flight, which is what makes
   * `savingSettings` an invariant rather than a convention the rows are
   * trusted to honour. Before both, switching detection off and then ticking
   * an app box switched detection back on. */
  patch: async (change) => {
    const base = get().status?.settings;
    if (base === undefined || get().savingSettings) return;
    set({ savingSettings: true });
    try {
      const status = await invoke<DetectionStatus>("detection_settings_set", {
        settings: { ...base, ...change },
      });
      get().setStatus(status);
    } finally {
      set({ savingSettings: false });
    }
  },

  /* Turning the calendar path on is what triggers the EventKit request, and
   * reading events needs full access. Asking first and only writing the
   * setting on success keeps the toggle from claiming a path that cannot run.
   *
   * The gate is held across the request, not just across the write: the
   * request ends by refreshing `status`, and a switch left live while macOS
   * has its dialog up can be turned off and then back on by the grant. */
  enableCalendar: async (enabled) => {
    if (!enabled) {
      set({ calendarRefused: false });
      await get().patch({ calendarEnabled: false });
      return;
    }
    if (get().savingSettings) return;
    set({ savingSettings: true });
    let authorized = false;
    try {
      authorized = (await get().requestCalendarAccess()) === "authorized";
    } finally {
      set({ savingSettings: false });
    }
    if (authorized) {
      await get().patch({ calendarEnabled: true });
      set({ calendarRefused: false });
      return;
    }
    /* macOS answers a decided permission without a dialog, so a refusal here
     * is invisible unless this store says it happened. The flag is
     * launch-local on purpose: System Settings is the only place that can
     * change the answer, and a fresh launch re-reads it. */
    set({ calendarRefused: true });
  },

  requestCalendarAccess: async () => {
    const access = await invoke<CalendarAccess>(
      "detection_calendar_access_request",
    );
    await get().refresh();
    return access;
  },

  requestNotificationAccess: async () => {
    const access = await invoke<NotificationAccess>(
      "detection_notification_access_request",
    );
    await get().refresh();
    return access;
  },
}));

/* Wires the two detection events into the store. Returns the detach function.
 *
 * Called once from the app shell, not per page: a prompt that arrives while the
 * operator is on another screen still has to reach the store, or clicking
 * "Start Transcribing" on a notification would silently do nothing. */
export const attachDetectionListeners = async (): Promise<() => void> => {
  const { setStatus, addPrompt } = useDetectionStore.getState();
  const unlisten = await Promise.all([
    listen<DetectionStatus>("detection-status", (event) => {
      setStatus(event.payload);
    }),
    listen<DetectionPromptEvent>("detection-prompt", (event) => {
      addPrompt(event.payload);
    }),
  ]);
  return () => {
    for (const detach of unlisten) detach();
  };
};
