import React from "react";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "@/components/vg/button";
import {
  Notice,
  SettingsRow,
  SettingsSection,
} from "@/components/settings/rows";
import { Switch } from "@/components/vg/switch";
import {
  useDetectionStore,
  type DetectionSettings,
  type DetectionStatus,
  type DetectionSuppressReason,
} from "./detectionStore";

const SUPPRESS_REASON_COPY = {
  detection_disabled: ["meetings.detection.why.disabled", "Detection is off."],
  sona_holds_input_device: [
    "meetings.detection.why.sonaHoldsMic",
    "Sona is using the microphone, so nothing else can be identified right now.",
  ],
  capture_already_active: [
    "meetings.detection.why.captureActive",
    "A meeting is already being captured.",
  ],
  no_qualifying_signal: [
    "meetings.detection.why.noSignal",
    "Nothing is using the microphone yet.",
  ],
  attendee_floor_not_met: [
    "meetings.detection.why.soloEvent",
    "The next event has fewer than two attendees, so it reads as blocked time.",
  ],
  unknown_mic_source: [
    "meetings.detection.why.unknownApp",
    "Something is using the microphone, but it is not a known meeting app.",
  ],
  browser_title_unreadable: [
    "meetings.detection.why.browserUnreadable",
    "A browser is in front, but its tab title cannot be read.",
  ],
  browser_title_not_meeting: [
    "meetings.detection.why.browserNotMeeting",
    "A browser is in front and its tab is not a call.",
  ],
  app_present_not_in_use: [
    "meetings.detection.why.appPresentNotInUse",
    "A meeting app is open, but you have not switched to it since the microphone came on.",
  ],
  sona_mic_just_closed: [
    "meetings.detection.why.sonaMicJustClosed",
    "The microphone Sona itself just used still reads as in use, so nothing else can be identified yet.",
  ],
} satisfies Record<DetectionSuppressReason, [string, string]>;

/* The one sentence the status line says for a suppressed tick. Exported as a
 * pure mapping, like `promptTitle`, because a static render cannot observe a
 * seeded store: this is where a reason's copy is checked. */
export const suppressReasonLine = (
  t: (key: string, fallback: string) => string,
  reason: DetectionSuppressReason,
): string =>
  t(SUPPRESS_REASON_COPY[reason][0], SUPPRESS_REASON_COPY[reason][1]);

export const adoptedCallLine = (
  t: (key: string, fallback: string, options: { app: string }) => string,
  displayName: string,
): string =>
  t(
    "meetings.detection.state.adoptedCall",
    "This recording stops when the {{app}} call ends.",
    { app: displayName },
  );

/** One degraded path, named. `live` belongs only to the line that changes on a
 *  tick rather than on something the operator did. */
interface DetectionStateLine {
  id: string;
  tone: "muted" | "warning";
  live: boolean;
  text: string;
}

const CALENDARS_PANE =
  "x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars";

export interface DetectionEditor {
  status: DetectionStatus | null;
  settings: DetectionSettings | null;
  saving: boolean;
  /* A decided macOS permission answers without a dialog; this is that answer. */
  calendarRefused: boolean;
  patch: (change: Partial<DetectionSettings>) => Promise<void>;
  enableCalendar: (enabled: boolean) => Promise<void>;
}

/* Detection's one write path, as the rows see it.
 *
 * Every field is store state, so this hook may be mounted as many times as
 * there are rows and there is still one write in flight, one gate, and one
 * base. It used to keep `saving` in component-local state and close `patch`
 * over a render-old `settings`, which made two adjacent Essentials rows two
 * independent writers of the same whole struct: turning the master switch off
 * and then ticking an app box sent `enabled: true` back and switched detection
 * on again. The gate that stops it belongs to the settings, not to a row, so
 * it lives beside the write in `detectionStore`. */
export const useDetectionEditor = (): DetectionEditor => {
  const status = useDetectionStore((state) => state.status);
  const saving = useDetectionStore((state) => state.savingSettings);
  const patch = useDetectionStore((state) => state.patch);
  const enableCalendar = useDetectionStore((state) => state.enableCalendar);
  const calendarRefused = useDetectionStore((state) => state.calendarRefused);

  return {
    status,
    settings: status?.settings ?? null,
    saving,
    calendarRefused,
    patch,
    enableCalendar,
  };
};

const DETECTION_ENABLED_ID = "detection-enabled";

/* The master switch, as an ordinary Essentials row.
 *
 * It used to ride in a section header, which is why the page then had to
 * repeat "Detect meetings" as both a heading and a control. On Essentials it is
 * one row among ten, so it says its name once.
 *
 * Unread state claims `true` because that is the backend default
 * (settings.rs `default_detection_enabled`): rendering "off" would invite a
 * click that turns working detection off. */
export const MeetingDetectionToggle: React.FC = () => {
  const { t } = useTranslation();
  const { settings, saving, patch } = useDetectionEditor();

  return (
    <SettingsRow
      label={t("settingsV2.essentials.detectMeetings")}
      /* The one thing the switch cannot say: noticing is not recording. */
      hint={t("settingsV2.essentials.detectMeetingsHint")}
      controlId={DETECTION_ENABLED_ID}
      disabled={settings === null}
    >
      <Switch
        id={DETECTION_ENABLED_ID}
        checked={settings?.enabled ?? true}
        onCheckedChange={(enabled) => void patch({ enabled })}
        disabled={settings === null || saving}
      />
    </SettingsRow>
  );
};

/* Everything detection can be told beyond "watch for meetings": the calendar
 * path behind its own permission, and the two choices that widen what counts
 * as evidence. Advanced > Meetings owns these; Essentials owns the switch that
 * makes them matter. */
export const MeetingDetectionAdvanced: React.FC = () => {
  const { t } = useTranslation();
  const { settings, saving, patch, enableCalendar, calendarRefused } =
    useDetectionEditor();

  if (settings === null) {
    return (
      <div className="px-6 py-3">
        <Notice tone="muted">
          {t("meetings.detection.loading", "Reading detection state…")}
        </Notice>
      </div>
    );
  }

  return (
    <>
      <SettingsRow
        label={t("meetings.detection.calendar.label", "Use my calendar")}
        /* The one thing about this switch nobody can infer from it: macOS
         * has no read-only calendar grant, so turning it on asks for the
         * whole calendar. */
        hint={t(
          "meetings.detection.calendar.description",
          "Shows a countdown a minute before events with two or more attendees. macOS asks for full calendar access the first time, because Apple offers no read-only grant.",
        )}
        controlId="detection-calendar"
        disabled={!settings.enabled}
      >
        <Switch
          id="detection-calendar"
          checked={settings.calendarEnabled}
          onCheckedChange={(enabled) => void enableCalendar(enabled)}
          disabled={!settings.enabled || saving}
        />
      </SettingsRow>

      {calendarRefused && (
        <SettingsRow
          label={t(
            "meetings.detection.calendar.refusedLabel",
            "Calendar access is limited",
          )}
          hint={t(
            "meetings.detection.calendar.refused",
            "macOS is letting Sona add events but not read them, and it only asks once. Choose Full Calendar Access for Sona in System Settings, then turn this on again.",
          )}
          controlId="detection-calendar-refused"
        >
          <Button
            variant="outline"
            size="sm"
            onClick={() => void openUrl(CALENDARS_PANE)}
          >
            {t(
              "meetings.detection.calendar.openSettings",
              "Open System Settings",
            )}
          </Button>
        </SettingsRow>
      )}

      <SettingsRow
        label={t(
          "meetings.detection.anyMic.label",
          "Ask on any microphone use",
        )}
        controlId="detection-any-mic"
        disabled={!settings.enabled}
      >
        <Switch
          id="detection-any-mic"
          checked={settings.anyMicActivity}
          onCheckedChange={(anyMicActivity) => void patch({ anyMicActivity })}
          disabled={!settings.enabled || saving}
        />
      </SettingsRow>

      {/* No open-on-countdown row. `autoStartOnOpenPane` is still on the wire
       * and still round-trips through the whole-struct write below, but the
       * consent slice took capture authority away from it in favour of
       * per-series standing consent — so a switch here would claim a decision
       * detection no longer reads. */}
    </>
  );
};

/* Silent detection is indistinguishable from broken detection, so every
 * degraded path names itself here, under the one line that always applies:
 * what actually stops a recording. That standing line is why this section
 * always renders. */
export const MeetingDetectionState: React.FC = () => {
  const { t } = useTranslation();
  const { status, settings } = useDetectionEditor();
  if (status === null || settings === null) return null;

  const stateLines: DetectionStateLine[] = [];
  if (settings.calendarEnabled && status.calendarAccess !== "authorized") {
    stateLines.push({
      id: "calendar",
      tone: "warning",
      live: false,
      text: t(
        "meetings.detection.state.calendarDenied",
        "Calendar access was not granted, so only the microphone path runs.",
      ),
    });
  }
  if (status.notificationAccess === "denied") {
    stateLines.push({
      id: "notifications",
      tone: "warning",
      live: false,
      text: t(
        "meetings.detection.state.notificationsDenied",
        "Notifications are off for Sona, so prompts appear in the app only.",
      ),
    });
  }
  if (status.inputDeviceReportingSuspect) {
    stateLines.push({
      id: "bluetooth",
      tone: "muted",
      live: false,
      text: t(
        "meetings.detection.state.bluetoothCaveat",
        "A meeting app is open but nothing reports using the microphone. Bluetooth headsets often do not, so start the meeting yourself if one is running.",
      ),
    });
  }
  if (status.suppressReason) {
    stateLines.push({
      id: "suppression",
      tone: "muted",
      live: true,
      text: suppressReasonLine(t, status.suppressReason),
    });
  }
  if (status.adoptedCall) {
    stateLines.push({
      id: "adoptedCall",
      tone: "muted",
      live: true,
      text: adoptedCallLine(t, status.adoptedCall.displayName),
    });
  }
  stateLines.push({
    id: "stopTriggers",
    tone: "muted",
    live: false,
    text: t(
      "meetings.detection.state.noSilenceStop",
      "Recording stops when the event ends, the app quits, your Mac sleeps, or you stop it yourself. Nothing stops it for silence: that would need live transcription, which only runs after a meeting ends.",
    ),
  });

  return (
    <SettingsSection
      label={t("meetings.detection.state.title", "What detection can see")}
    >
      <div className="flex flex-col gap-1.5 px-6 py-3">
        {stateLines.map((line) => (
          <Notice key={line.id} tone={line.tone} live={line.live}>
            {line.text}
          </Notice>
        ))}
      </div>
    </SettingsSection>
  );
};
