import React from "react";
import { useTranslation } from "react-i18next";
import type { SourceKind } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { promptTitle } from "./DetectionListeners";
import {
  useDetectionStore,
  type CalendarEventSummary,
  type DetectionPromptKind,
} from "./detectionStore";
import {
  MeetingPreviewCard,
  MeetingPreviewList,
  eventFacts,
} from "./MeetingPreviewCard";
import { useSeriesTemplate } from "./seriesTemplate";

/* The pre-meeting pane from §5.3 case 1, plus any prompt still waiting.
 *
 * Renders nothing when there is nothing to say, so it costs a page no vertical
 * space in the common case. Two things share one surface deliberately: a
 * countdown and a prompt are the same question at two moments, and splitting
 * them would put two competing affordances on the same screen.
 *
 * Having this card open for an event is also what §5.3 case 2 reads as prior
 * opt-in, so its visibility is load-bearing, not decorative. That is also why
 * the countdown carries no Skip: the backend reads "we published a countdown
 * for this event", not "the card is on screen", so a local dismiss would hide
 * the card while leaving the carve-out armed. The NOTIFY row's switch is the
 * real control for that, and it writes the real setting.
 *
 * A prompt is an object with an answer attached, so it gets a surface. The
 * pane carries no sentence of its own: the countdown chip states when, the
 * card's own Start states that nothing happens until it is pressed, and the
 * page's one assurance sentence sits under its title. */

export interface PreMeetingCountdownCardProps {
  /** What the next capture will request. Read-only here: the page has one
   * answer to that question and Settings owns it. */
  sources: SourceKind[];
  starting: boolean;
  /** Routes a calendar event into the page's existing start path, which
   * creates a preflight and puts the consent screen in front of the
   * operator. */
  onStartEvent: (event: CalendarEventSummary) => void;
}

/** The application a prompt names, for prompts that name one. A calendar
 * prompt names an event instead, and an unknown microphone source names
 * nothing at all — both leave the APP row off the card. */
const promptAppName = (prompt: DetectionPromptKind): string | null => {
  switch (prompt.kind) {
    case "AppMeeting":
    case "AppHuddle":
    case "AppCall":
    case "BrowserCall":
      return prompt.appName;
    case "CalendarEvent":
    case "UnknownMicSource":
      return null;
  }
};

export const PreMeetingCountdownCard: React.FC<
  PreMeetingCountdownCardProps
> = ({ sources, starting, onStartEvent }) => {
  const { t } = useTranslation();
  const status = useDetectionStore((state) => state.status);
  const prompts = useDetectionStore((state) => state.prompts);
  const answer = useDetectionStore((state) => state.answer);
  const patch = useDetectionStore((state) => state.patch);
  const savingSettings = useDetectionStore((state) => state.savingSettings);
  const notesTemplate = useSettingsStore(
    (state) => state.settings?.meeting_notes_template ?? null,
  );

  const countdown = status?.countdown ?? null;
  /* D21: what the next meeting in this series will actually be shaped into.
   * The hook is called before the early return below, because a hook cannot be
   * conditional — it answers null for an event with no series, which is the
   * same thing the early return would have meant. */
  const seriesTemplate = useSeriesTemplate(countdown?.event.seriesKey ?? null);
  const countdownTemplate = seriesTemplate?.template ?? notesTemplate;
  if (countdown === null && prompts.length === 0) return null;

  const recording = { armed: sources };

  /* The same whole-struct write the detection rows use, so this switch cannot
   * revert one of theirs — or be reverted by it — while both are on screen. */
  const setAutoOpen = (next: boolean) =>
    void patch({ autoStartOnOpenPane: next });

  return (
    <MeetingPreviewList
      label={t("meetings.detection.pane.title", "Starting soon")}
    >
      {countdown && status ? (
        <MeetingPreviewCard
          key={countdown.event.eventKey}
          facts={eventFacts(countdown.event, t)}
          secondsToStart={countdown.secondsToStart}
          briefing={countdown.briefing}
          /* The countdown is the one card worth opening on arrival: it is on
           * screen because something is about to start, and every row on it
           * is a decision that expires. */
          defaultExpanded
          notify={{
            access: status.notificationAccess,
            delivered: null,
            autoOpen: {
              checked: status.settings.autoStartOnOpenPane,
              onChange: setAutoOpen,
              disabled: savingSettings,
            },
          }}
          recording={recording}
          notesTemplate={countdownTemplate}
          notesTemplateFromSeries={seriesTemplate?.template != null}
          starting={starting}
          onStart={() => onStartEvent(countdown.event)}
        />
      ) : null}

      {prompts.map((prompt) => (
        <MeetingPreviewCard
          key={prompt.promptId}
          facts={{
            id: prompt.promptId,
            title: promptTitle(t, prompt.prompt),
            origin: prompt.prompt.kind === "CalendarEvent" ? "calendar" : "app",
            startUtcMs: null,
            endUtcMs: null,
            calendarName: null,
            appName: promptAppName(prompt.prompt),
            attendeeCount: null,
            participants: [],
            description: null,
            url: null,
          }}
          notify={
            status === null
              ? null
              : {
                  access: status.notificationAccess,
                  delivered: prompt.delivery !== "in_app_only",
                  autoOpen: null,
                }
          }
          recording={recording}
          notesTemplate={notesTemplate}
          starting={starting}
          onStart={() => void answer(prompt.promptId, true)}
          onSkip={() => void answer(prompt.promptId, false)}
        />
      ))}
    </MeetingPreviewList>
  );
};
