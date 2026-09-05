import type { Page } from "@playwright/test";

import {
  APP_SETTINGS,
  MODES_SNAPSHOT,
  meetingStartedAtMs,
} from "./tauri-fixtures";

/**
 * Playwright runs the frontend in a plain Chromium, so every `invoke` and every
 * Tauri plugin internal has to be provided here. Without the OS and event plugin
 * internals the app throws while mounting, and without a non-null model list the
 * model selector crashes on `find`.
 */
/** Any JSON-serializable command response; the payload crosses the Playwright
 * serialization boundary, so JSON is the exact value contract. */
export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export type TauriMockOptions = {
  /** Command responses layered over the defaults. An override always wins. */
  responses?: Record<string, JsonValue>;
  /** Backend events delivered once their matching listener registers. */
  events?: Record<string, JsonValue[]>;
  /** Commands that stay in flight for launch-order assertions. */
  pending?: string[];
};

/** Installs the mocked Tauri runtime before any page script runs. */
export async function installTauriMock(
  page: Page,
  options: TauriMockOptions = {},
): Promise<void> {
  await page.addInitScript(installMockedRuntime, {
    settings: APP_SETTINGS,
    modes: MODES_SNAPSHOT,
    startedAtUtcMs: meetingStartedAtMs(),
    responses: options.responses ?? {},
    events: {
      "launch:backend-ready": [null],
      ...options.events,
    },
    pending: options.pending ?? [],
  });
}

type MockPayload = {
  settings: Record<string, JsonValue>;
  modes: Record<string, JsonValue>;
  /** When the mocked capturing session started, in UTC milliseconds. */
  startedAtUtcMs: number;
  responses: Record<string, JsonValue>;
  events: Record<string, JsonValue[]>;
  pending: string[];
};

/** The envelope every mocked `plugin:event|listen` callback receives; the
 * payload side is JSON by the same serialization contract as `JsonValue`. */
type MockEventEnvelope = { event: string; id: number; payload: JsonValue };

type RecorderMockState = {
  phase: string;
  elapsedMs: number;
  screenSelected: boolean;
  droppedVideoFrames: number;
  outputPath: string | null;
  width: number | null;
  height: number | null;
  failure: string | null;
};

/**
 * Runs in the browser, so it may not close over anything outside its argument:
 * Playwright serializes the function and passes `payload` as JSON.
 *
 * Exported so a screenshot harness can serialize the same runtime into a plain
 * `<script>` ahead of the app bundle. One owner for what the browser sees.
 */
export function installMockedRuntime(payload: MockPayload): void {
  const state = { phase: "preflight", started: 0, stopped: 0 };
  localStorage.setItem("meeting-started", "0");
  localStorage.setItem("meeting-stopped", "0");
  localStorage.setItem("detection-panel-ack", "");
  localStorage.setItem("detection-prompt-response", "");
  const callbacks = new Map<number, (value: MockEventEnvelope) => void>();
  /* Which callbacks are listening to which backend event, so a spec can push
   * an update after the page has already asked for it — the `events` option
   * only replays at the moment a listener registers, which cannot reach a
   * job that did not exist yet. */
  const listeners = new Map<string, Set<number>>();
  const pendingCommands = new Set(payload.pending);
  let nextCallbackId = 1;

  /* Wire-boundary parsers: the mock validates request shapes the way the real
   * backend's serde layer would — decode into the domain, never narrow by
   * representation. A string is the one JSON value identical to its own
   * String() image. */
  const jsonObject = (
    value: JsonValue | undefined,
  ): { [key: string]: JsonValue } | null =>
    value instanceof Object && !Array.isArray(value) ? value : null;
  const isJsonString = (value: JsonValue | undefined): value is string =>
    value === String(value ?? "");

  /* The backend opens one job per imported file and the frontend follows each
   * of them by id, so a mock that answered every `import_audio_file` with the
   * same id would let three rows share one job's status. */
  let audioImportJobs = 0;

  const recorder: RecorderMockState = {
    phase: "idle",
    elapsedMs: 0,
    screenSelected: false,
    droppedVideoFrames: 0,
    outputPath: null,
    width: null,
    height: null,
    failure: null,
  };
  const resetRecorder = () => {
    recorder.phase = "idle";
    recorder.elapsedMs = 0;
    recorder.screenSelected = false;
    recorder.droppedVideoFrames = 0;
    recorder.outputPath = null;
    recorder.width = null;
    recorder.height = null;
    recorder.failure = null;
  };

  const recorderSnapshot = () => ({ ...recorder });
  const recorderPreflight = () => ({
    availability: "supported",
    startAvailability: "ready",
    cameraDevices: [{ id: "camera-default", name: "Built-in Camera" }],
    microphoneDevices: [
      { id: "microphone-default", name: "MacBook Pro Microphone" },
    ],
  });
  const validateRecorderPreviewRequest = (value: JsonValue | undefined) => {
    const request = jsonObject(value);
    if (
      request === null ||
      (request.cameraEnabled !== true && request.cameraEnabled !== false) ||
      (request.microphoneEnabled !== true &&
        request.microphoneEnabled !== false) ||
      (request.cameraDeviceId !== null &&
        !isJsonString(request.cameraDeviceId)) ||
      (request.microphoneDeviceId !== null &&
        !isJsonString(request.microphoneDeviceId)) ||
      (request.cameraEnabled !== true && request.cameraDeviceId !== null) ||
      (request.microphoneEnabled !== true &&
        request.microphoneDeviceId !== null)
    ) {
      throw new Error("recorder_preview_start received an invalid request");
    }
    for (const key of Object.keys(request)) {
      if (
        key !== "cameraEnabled" &&
        key !== "cameraDeviceId" &&
        key !== "microphoneEnabled" &&
        key !== "microphoneDeviceId"
      ) {
        throw new Error("recorder_preview_start received an unknown option");
      }
    }
  };

  const sources = [
    {
      track_id: null,
      source_kind: "microphone",
      required: true,
      availability: "available",
      health: "healthy",
      format: null,
      last_durable_offset_ns: 0,
      gap_count: 0,
    },
    {
      track_id: null,
      source_kind: "system_audio",
      required: true,
      availability: "available",
      health: "healthy",
      format: null,
      last_durable_offset_ns: 0,
      gap_count: 0,
    },
  ];

  const capturing = () =>
    state.phase === "capturing_recording" || state.phase === "capturing_paused";

  const session = () => ({
    session_id: "meeting-1",
    phase: state.phase,
    revision: capturing() ? 2 : 1,
    title: "Local notes",
    started_at_utc_ms: capturing() ? payload.startedAtUtcMs : null,
    elapsed_offset_ns: capturing() ? 8_000_000_000 : null,
    sources,
    open_capture_window_started_at_ns: null,
    capture_completeness: "not_started",
    storage: "available",
    processing_status: { kind: "pending" },
    preflight_local_processing: capturing() ? null : "available",
    retention_deadline_utc_ms: null,
    allowed_actions: capturing()
      ? ["pause", "stop", "discard"]
      : ["refresh_preflight", "cancel_preflight", "start"],
  });

  const receipt = () => ({
    schema_version: 1,
    operation_id: "operation-1",
    session_id: "meeting-1",
    actor: "user",
    command: "start",
    expected_revision: 1,
    from_phase: "preflight",
    to_phase: state.phase,
    requested_at_utc_ms: 1_756_136_400_000,
    committed_at_utc_ms: 1_756_136_400_000,
    result: "committed",
    reason_codes: [],
    new_revision: capturing() ? 2 : 1,
    effect_ids: [],
  });

  /* Detection defaults to a quiet status. Specs can inject prompts through the
   * event map without needing a real CoreAudio transition or calendar TCC. */
  const detectionStatus = () => ({
    eventSchemaVersion: 2,
    settings: {
      enabled: true,
      calendarEnabled: false,
      anyMicActivity: false,
      autoStartOnOpenPane: false,
      meetingApps: [
        "us.zoom.xos",
        "com.microsoft.teams2",
        "com.microsoft.teams",
        "com.tinyspeck.slackmacgap",
        "com.webex.meetingmanager",
      ],
    },
    calendarAccess: "not_determined",
    notificationAccess: "not_determined",
    inputDeviceActive: false,
    sonaHoldsInputDevice: false,
    suppressReason: "no_qualifying_signal",
    countdown: null,
    runningMeetingApps: [],
    availableStopTriggers: [
      "sleep_boundary",
      "event_end",
      "trigger_app_exited",
      "input_device_idle",
    ],
    inputDeviceReportingSuspect: false,
  });

  const userNotes = () => ({
    session_id: "meeting-1",
    body: "",
    template: "general",
    revision: 0,
    updated_at_utc_ms: 1_756_136_400_000,
  });

  /* The zero-valued snapshot the analytics strip renders its "nothing to
   * measure" state from. */
  const analyticsSnapshot = () => ({
    session_id: "meeting-1",
    input_revision: 1,
    computed_at_utc_ms: 1_756_136_400_000,
    analytics: {
      talk: {
        segment_count: 0,
        turn_count: 0,
        interaction_count: 0,
        total_speaking_ns: 0,
        speakers: [],
        longest_monologue_ns: 0,
        longest_monologue_speaker_id: null,
        median_switch_gap_ms: null,
      },
      trackers: [],
    },
    action_items: [],
    notes: userNotes(),
  });

  /* The chat sheet's quiet-but-healthy snapshot: relay paired and reachable,
   * nothing in flight. `chatPhase` (src/components/chat/chatModel.ts) maps
   * `ready` to the one phase a browser can reach — the mock answers
   * `plugin:event|listen` with an id and then emits nothing, so no turn or
   * proposal lifecycle ever advances on its own. A spec that needs one supplies
   * it through `responses`. Shapes are AgentPanelStatusV1 in src/bindings.ts. */
  const agentPanel = () => ({
    invalidation_id: 1,
    relay_status: "ready",
    conversation_id: null,
    conversation: [],
    turn: null,
    proposal: null,
  });

  const defaults = new Map<string, JsonValue>([
    ["get_app_settings", payload.settings],
    ["get_settings", payload.settings],
    ["get_default_settings", payload.settings],
    ["get_modes", payload.modes],
    ["plugin:os|platform", "macos"],
    [
      "discover_post_process_model_catalog",
      {
        provider_id: "openai",
        models: [{ id: "gpt-4o-mini", provenance: "provider_reported" }],
        discovery: "ready",
        allows_manual_model_id: true,
      },
    ],
    /* getVersion() feeds the What's New gate and the About page. Unanswered it
       resolves to null, and the gate then logs "Failed to load release notes"
       from a version parse that never sees a string. Kept in step with
       src-tauri/tauri.conf.json's version. */
    ["plugin:app|version", "1.1.0"],
    ["plugin:event|listen", 1],
    ["plugin:event|unlisten", null],
    ["get_available_microphones", []],
    ["get_available_output_devices", []],
    ["get_available_models", []],
    ["get_current_model", ""],
    /* The store keeps this object non-optional and SoundPicker reads its two
       flags on every render, so the mock answers with the wire shape rather
       than the null an unmocked command falls through to. */
    ["check_custom_sounds", { start: false, stop: false }],
    ["get_transcription_model_status", null],
    ["is_model_loading", false],
    ["is_recording", false],
    [
      "get_history_entries",
      {
        entries: [],
        has_more: false,
        total: 0,
        total_count: 0,
      },
    ],
    [
      "search_history_entries",
      {
        entries: [],
        has_more: false,
        total: 0,
        total_count: 0,
      },
    ],
    ["get_history_stats", null],
    ["get_history_trend", null],
    ["get_history_run_receipts", []],
    /* Capture's instrument strip: the engine's real binding and the device's
     * real channel count. `backend` is the field `ModelLoadStatus` gained this
     * wave — the compute backend the loaded engine bound to, not the requested
     * accelerator. */
    [
      "get_model_load_status",
      { is_loaded: false, current_model: null, backend: null },
    ],
    ["get_microphone_channels", 1],
    ["list_vocabulary_entries", []],
    ["list_audio_import_jobs", []],
    /* The native file picker. Unanswered it falls through to null, which the
     * import dialog reads as a dismissed picker, so a spec that clicks
     * "Choose file" would see nothing happen. A spec that wants a dismissal —
     * or different files — overrides `plugin:dialog|open` through `responses`.
     * `multiple: true` means an array, which is what the callers with
     * `multiple: false` already guard against. */
    [
      "plugin:dialog|open",
      [
        "/Users/sona/Recordings/standup-2026-01-12.m4a",
        "/Users/sona/Recordings/design-review-part-two.wav",
        "/Users/sona/Recordings/customer-call.mp3",
      ],
    ],
    ["list_snippets", []],
    [
      "check_for_updates",
      {
        current_version: "1.0.0",
        latest_version: null,
        update_available: false,
        url: null,
        notes_excerpt: null,
        published_at_utc_ms: null,
        status: "up_to_date",
        error: null,
      },
    ],
    [
      "cloud_sync_service_status",
      {
        configured: false,
        endpoint: null,
        reason: "no endpoint configured",
      },
    ],
    ["cloud_sync_overview_get", null],
    ["get_agent_bridge_status", null],
    ["get_secure_input_status", null],
    ["get_context_diagnostics", null],
    ["meeting_suggestions_list", []],
    ["meeting_list", { entries: [], has_more: false }],
    ["meeting_recovery_list", []],
    ["meeting_trash_list", []],
    ["meeting_retention_get", { policy: { kind: "forever" }, revision: 1 }],
    ["meeting_trend", null],
    ["people_list", { schema_version: 1, revision: 1, entries: [] }],
    ["meeting_people_context", { schema_version: 1, revision: 1, rows: [] }],
    [
      "person_split",
      {
        schema_version: 1,
        revision: 2,
        person: {
          id: "person-split",
          display_name: "Split person",
          aliases: [],
          calendar_emails: [],
          created_at_utc_ms: 1_756_136_400_000,
          updated_at_utc_ms: 1_756_136_400_000,
        },
        removed: false,
      },
    ],
    ["doc_list", { schema_version: 1, revision: 1, entries: [] }],
    [
      "doc_ingest",
      {
        schema_version: 1,
        revision: 2,
        document: null,
        removed: false,
      },
    ],
    ["open_loops_inbox", { schema_version: 1, revision: 1, entries: [] }],
    ["workflows_list", { schema_version: 1, revision: 1, entries: [] }],
    [
      "workflow_runs",
      {
        schema_version: 1,
        revision: 1,
        entries: [],
        next_cursor: null,
      },
    ],
    ["learning_suggestions", { schema_version: 1, revision: 1, entries: [] }],
    ["learning_decide", { schema_version: 1, revision: 2, entries: [] }],

    // Meeting detection (MeetingDetect).
    ["detection_status_get", detectionStatus()],
    ["detection_settings_set", detectionStatus()],
    ["detection_calendar_access_request", "not_determined"],
    ["detection_notification_access_request", "not_determined"],
    ["detection_prompt_respond", null],
    ["detection_prompt_panel_ack", null],
    ["meeting_ritual_panel_ack", null],
    ["meeting_ritual_respond", true],
    [
      "meeting_follow_up_draft",
      {
        session_id: "00000000-0000-0000-0000-000000000020",
        title: "Weekly product review",
        source: "structured",
        message: "Thanks for the meeting. I will send the launch plan.",
        summary: "Launch plan agreed.",
        mine: ["Send the launch plan"],
        decisions: [],
        receipt: receipt(),
      },
    ],
    ["detection_running_meeting_apps", []],
    ["meeting_consent_panel_forget_series", true],

    // Meeting analytics + notes (MeetingAnalyticsNotes).
    ["get_meeting_analytics", analyticsSnapshot()],
    ["list_keyword_trackers", []],
    ["save_keyword_trackers", []],
    ["set_action_item_done", []],
    ["get_meeting_user_notes", userNotes()],
    ["save_meeting_user_notes", userNotes()],
    [
      "reenhance_meeting_with_notes",
      { receipt: receipt(), snapshot: session() },
    ],
    [
      "meeting_catch_up",
      {
        state: "no_transcript_yet",
        bullets: [],
        through_offset_ns: null,
        segment_count: 0,
      },
    ],

    // History power pack (HistoryPowerPackV2).
    ["reprocess_history_entry", null],
    ["get_text_replacements", []],
    ["save_text_replacements", []],
    ["reset_text_replacements", []],
    ["update_text_replacements_enabled", null],
    ["get_persona_samples", []],
    ["save_persona_samples", []],
    [
      "hud_pill_state",
      { enabled: false, position: "bottom", mode_name: null, mode_id: null },
    ],
    ["set_hud_pill_enabled", null],
    ["set_hud_pill_position", null],
    ["hud_toggle_recording", null],
    ["hud_open_mode_menu", null],

    // Command mode (CommandModeContext / SettingsOnboardingRestyle).
    ["change_command_mode_enabled_setting", null],

    /* Appearance material (TokensTypeFoundation). Rust resolves intent against
     * whether native vibrancy applied and writes `data-material` itself, so the
     * command answers with nothing. */
    ["change_appearance_material_setting", null],

    /* The chat sheet, inside the main window. Every commands.agentPanel* and
     * commands.agentChat* call site branches on the `{ status: "ok" | "error" }`
     * result shape, so an unmocked command falling through to `return null` is
     * a crash on `result.data.relay_status`, not a graceful miss. They answer
     * with the same quiet snapshot except for the field each one moves. */
    ["agent_panel_status", agentPanel()],
    /* Post-enqueue, which is what the backend returns before any turn event:
     * the sheet shows a running turn and stays there, because nothing here
     * emits agent-panel://turn-changed. */
    [
      "agent_panel_send_turn",
      {
        ...agentPanel(),
        conversation_id: "conversation-1",
        turn: {
          turn_id: "chat-turn-1",
          workspace: "sona_chat",
          state: "queued",
          event_cursor: 0,
          started_at_utc_ms: 0,
          completed_at_utc_ms: null,
          steps: [],
        },
      },
    ],
    [
      "agent_panel_cancel_turn",
      {
        ...agentPanel(),
        conversation_id: "conversation-1",
        turn: {
          turn_id: "chat-turn-1",
          workspace: "sona_chat",
          state: "canceled",
          event_cursor: 1,
          started_at_utc_ms: 0,
          completed_at_utc_ms: 1000,
          steps: [],
        },
      },
    ],
    /* Both resolve the pending proposal, so the snapshot comes back with none.
     * A spec that needs the applied/undone preview — to reach the undo
     * affordance — supplies its own AgentPanelProposalPreviewV1. */
    ["agent_panel_apply_change", agentPanel()],
    ["agent_panel_undo_change", agentPanel()],
    /* The history button reads the list on open; nothing has been asked yet in
     * a fresh browser, so it is empty. */
    ["agent_chat_history_list", []],
    ["agent_chat_open", agentPanel()],
    ["agent_chat_new", agentPanel()],
    [
      "agent_panel_public_identity",
      { key_id: "sona-key-1", public_key: "0".repeat(64) },
    ],

    /* The one query plane (⌘K). Two characters into the palette's field the
     * surface asks `sona_query_search`, so an unmocked command would answer
     * `null` and every spec that types would read the palette's "search is
     * unavailable" notice instead of its command list. The empty page is the
     * exact shape `query/mod.rs` returns; a spec that wants rows supplies them
     * through `responses`, because a default with rows in it would silently
     * change what "no commands found" means for every other spec.
     *
     * `sona_open_link` answers `true` — the address was ours — and routes
     * nothing: navigation happens through the events the backend emits, which
     * a spec injects through the event map. */
    [
      "sona_query_search",
      { schema_version: 1, entries: [], next_cursor: null },
    ],
    [
      "sona_query_pack",
      {
        schema_version: 1,
        pack: "sona context pack 1\nquestion: what did I promise\nquotes: 0 of 0",
        sources: [],
      },
    ],
    ["sona_open_link", true],
  ]);

  const invoke = async (
    command: string,
    args?: Record<string, JsonValue>,
  ): Promise<JsonValue> => {
    const invokeCountKey = `tauri-invoke:${command}`;
    const invokeCount = Number(localStorage.getItem(invokeCountKey) ?? "0");
    localStorage.setItem(invokeCountKey, String(invokeCount + 1));
    /* Which chat the column asked for, recorded beside the counter because
     * `agent_chat_open` is answered out of `responses` before any branch
     * below runs, and one static snapshot cannot say which row was pressed. */
    if (command === "agent_chat_open") {
      localStorage.setItem(
        "agent-chat-opened",
        String(args?.conversationId ?? ""),
      );
    }
    if (command === "plugin:event|listen") {
      const eventName = String(args?.event ?? "");
      const callbackId = Number(args?.handler);
      const callback = callbacks.get(callbackId);
      if (callback !== undefined) {
        const live = listeners.get(eventName) ?? new Set<number>();
        live.add(callbackId);
        listeners.set(eventName, live);
        window.setTimeout(() => {
          for (const eventPayload of payload.events[eventName] ?? []) {
            callback({
              event: eventName,
              id: callbackId,
              payload: eventPayload,
            });
          }
        }, 0);
      }
      return callbackId;
    }
    if (pendingCommands.has(command)) {
      return new Promise<JsonValue>(() => undefined);
    }
    if (Object.prototype.hasOwnProperty.call(payload.responses, command)) {
      return payload.responses[command];
    }
    /* A queued job named after the file it will decode, one per call, as
     * `AudioImportJob` in src/bindings.ts. A spec drives it forward by
     * emitting `audio-import-update-event` payloads carrying the same id. */
    if (command === "import_audio_file") {
      const path = String(args?.path ?? "");
      audioImportJobs += 1;
      return {
        id: audioImportJobs,
        file_name: path.slice(path.lastIndexOf("/") + 1),
        status: "queued",
        decoded_samples: 0,
        cancel_requested: false,
        result: null,
      };
    }
    if (command === "recorder_preflight") return recorderPreflight();
    if (command === "recorder_preview_start") {
      validateRecorderPreviewRequest(args?.request);
      recorder.phase = "previewing";
      recorder.elapsedMs = 0;
      recorder.screenSelected = true;
      recorder.droppedVideoFrames = 0;
      recorder.outputPath = null;
      recorder.width = null;
      recorder.height = null;
      recorder.failure = null;
      return recorderSnapshot();
    }
    if (command === "recorder_preview_stop") {
      if (recorder.phase !== "previewing") {
        throw new Error("recorder_preview_stop requires a native preview");
      }
      resetRecorder();
      return recorderSnapshot();
    }
    /* Cancel is legal from previewing, recording and paused (`cancel_reaches`
     * in recorder.rs); only preview_stop is previewing-only (`preview_stop`
     * in the same file). Folding the two into one branch once taught the mock
     * a phase rule the backend does not have. */
    if (command === "recorder_cancel") {
      if (
        recorder.phase !== "previewing" &&
        recorder.phase !== "recording" &&
        recorder.phase !== "paused"
      ) {
        throw new Error("recorder_cancel requires an active capture");
      }
      resetRecorder();
      return recorderSnapshot();
    }
    if (command === "recorder_start") {
      if (recorder.phase !== "previewing") {
        throw new Error("recorder_start requires a native preview");
      }
      recorder.phase = "recording";
      return recorderSnapshot();
    }
    if (command === "recorder_pause") {
      if (recorder.phase !== "recording") {
        throw new Error("recorder_pause requires an active recording");
      }
      recorder.phase = "paused";
      recorder.elapsedMs = 12_000;
      return recorderSnapshot();
    }
    if (command === "recorder_resume") {
      if (recorder.phase !== "paused") {
        throw new Error("recorder_resume requires a paused recording");
      }
      recorder.phase = "recording";
      return recorderSnapshot();
    }
    if (command === "recorder_stop") {
      if (recorder.phase !== "recording" && recorder.phase !== "paused") {
        throw new Error("recorder_stop requires an active recording");
      }
      recorder.phase = "saved";
      /* The published name the recorder writes: sona-screen-<UTC>-<uuid>.mp4
       * (recorder.rs:713-745). A fixed timestamp and id keep it assertable. */
      recorder.outputPath =
        "/recordings/sona-screen-20260904T101500Z-6f1c9b2e-77d3-4a8f-9a1c-2b7e4d5f6a80.mp4";
      recorder.width = 1920;
      recorder.height = 1080;
      return recorderSnapshot();
    }
    if (
      command === "meeting_preflight_create" ||
      command === "meeting_preflight_refresh"
    ) {
      if (command === "meeting_preflight_create") {
        // SAFETY: the generated meeting_preflight_create binding always puts
        // MeetingPreflightCreateRequest at args.request.
        const request = args?.request as
          | { calendar_event_key?: string | null }
          | undefined;
        const calendarEventKey = request?.calendar_event_key ?? "";
        localStorage.setItem("meeting-calendar-event-key", calendarEventKey);
      }
      return { receipt: receipt(), snapshot: session() };
    }
    if (command === "meeting_get") {
      return {
        session: session(),
        tracks: [],
        gaps: [],
        speakers: [],
        transcript: [],
        notes: [],
        artifacts: [],
        questions: [],
        diarization: {
          status: "not_requested",
          model_id: "local",
          model_version: "1",
          generation_id: null,
          assigned_segment_count: 0,
        },
        can_export: false,
        remote_cancellation_pending: false,
      };
    }
    if (command === "meeting_start") {
      state.started += 1;
      state.phase = "capturing_recording";
      localStorage.setItem("meeting-started", String(state.started));
      return { receipt: receipt(), snapshot: session() };
    }
    if (command === "meeting_consent_panel_start") {
      const request = jsonObject(args?.request);
      const alwaysRecord = request?.always_record_series;
      const consent = jsonObject(request?.consent);
      if (
        request === null ||
        !isJsonString(request.prompt_id) ||
        !isJsonString(request.operation_id) ||
        (alwaysRecord !== true && alwaysRecord !== false) ||
        consent === null
      ) {
        throw new Error(
          "meeting_consent_panel_start received an invalid request",
        );
      }
      localStorage.setItem(
        "meeting-consent-panel-start-request",
        JSON.stringify(request),
      );
      state.started += 1;
      state.phase = "capturing_recording";
      localStorage.setItem("meeting-started", String(state.started));
      return { receipt: receipt(), snapshot: session() };
    }
    if (command === "meeting_stop") {
      const request = jsonObject(args?.request);
      if (
        request === null ||
        !isJsonString(request.session_id) ||
        !Number.isFinite(request.expected_revision)
      ) {
        throw new Error("meeting_stop received an invalid request");
      }
      // The surface is the whole point of the argument: a caller that leaves
      // it out files its press as an anonymous operator stop, which is the
      // state this replaced.
      if (
        args?.surface !== "meeting_live" &&
        args?.surface !== "consent_panel"
      ) {
        throw new Error("meeting_stop requires the pressing surface");
      }
      state.stopped += 1;
      state.phase = "review_ready";
      localStorage.setItem("meeting-stopped", String(state.stopped));
      return { receipt: receipt(), snapshot: session() };
    }
    if (command === "detection_prompt_panel_ack") {
      const promptId = args?.promptId;
      if (!isJsonString(promptId)) {
        throw new Error("detection_prompt_panel_ack requires promptId");
      }
      localStorage.setItem("detection-panel-ack", promptId);
      return null;
    }
    if (command === "meeting_ritual_panel_ack") {
      const ritualId = args?.ritualId;
      if (!isJsonString(ritualId)) {
        throw new Error("meeting_ritual_panel_ack requires ritualId");
      }
      localStorage.setItem("meeting-ritual-panel-ack", ritualId);
      return null;
    }
    if (command === "meeting_ritual_respond") {
      const ritualId = args?.ritualId;
      const action = args?.action;
      if (!isJsonString(ritualId) || !isJsonString(action)) {
        throw new Error("meeting_ritual_respond requires a ritual response");
      }
      localStorage.setItem(
        "meeting-ritual-response",
        JSON.stringify({ ritualId, action }),
      );
      return true;
    }
    if (command === "detection_prompt_respond") {
      const promptId = args?.promptId;
      const accepted = args?.accepted;
      if (
        !isJsonString(promptId) ||
        (accepted !== true && accepted !== false)
      ) {
        throw new Error(
          "detection_prompt_respond received an invalid response",
        );
      }
      localStorage.setItem(
        "detection-prompt-response",
        JSON.stringify({ promptId, accepted }),
      );
      return null;
    }
    if (command === "meeting_consent_panel_active_state") {
      /* `disclosure` is not optional on the wire: the panel reads its kind on
       * every render of the recording pill. */
      return capturing()
        ? {
            snapshot: session(),
            standing_series_key: null,
            disclosure: { kind: "not_asked" },
          }
        : null;
    }
    /* The agent-bridge observation reads return lists in production; a null
     * here crashes the console's memos, so the mock honors the contract. */
    if (
      command === "get_agent_bridge_sessions" ||
      command === "get_agent_bridge_requests" ||
      command === "get_agent_bridge_pending_messages"
    ) {
      return [];
    }
    const preset = defaults.get(command);
    if (preset !== undefined) {
      return preset;
    }
    if (command.includes("permission")) return true;
    return null;
  };

  type TauriGlobals = {
    isTauri: boolean;
    __TAURI_INTERNALS__: {
      invoke: typeof invoke;
      transformCallback: (
        callback?: (value: MockEventEnvelope) => void,
      ) => number;
      convertFileSrc: (path: string) => string;
    };
    __TAURI_OS_PLUGIN_INTERNALS__: Record<string, string>;
    __TAURI_EVENT_PLUGIN_INTERNALS__: { unregisterListener: () => void };
    /** Pushes one backend event to whoever is listening for it right now. */
    sonaMockEmit: (event: string, eventPayload: JsonValue) => void;
  };
  // SAFETY: the mock plants the Tauri globals the runtime reads at startup;
  // Window does not declare them, so the intersection assertion is the way in.
  const target = window as Window & Partial<TauriGlobals>;
  target.isTauri = true;
  target.__TAURI_INTERNALS__ = {
    invoke,
    transformCallback: (callback) => {
      const id = nextCallbackId++;
      if (callback !== undefined) callbacks.set(id, callback);
      return id;
    },
    convertFileSrc: (path: string) => path,
  };
  target.__TAURI_OS_PLUGIN_INTERNALS__ = {
    platform: "macos",
    arch: "aarch64",
    family: "unix",
    os_type: "macos",
    ostype: "macos",
    version: "15.6.0",
    exe_extension: "",
    eol: "\n",
  };
  target.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  target.sonaMockEmit = (event, eventPayload) => {
    for (const id of listeners.get(event) ?? []) {
      callbacks.get(id)?.({ event, id, payload: eventPayload });
    }
  };
}
