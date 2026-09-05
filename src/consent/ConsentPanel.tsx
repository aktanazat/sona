"use client";

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactElement,
  type ReactNode,
} from "react";
import { FocusScope } from "radix-ui/internal";
import { Mic } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import {
  commands,
  events,
  type DetectionPromptEvent,
  type DetectionStatus,
  type MeetingConsentPanelSessionState,
  type MeetingPrepCard,
  type MeetingRecordingCard,
  type MeetingRitualAction,
  type MeetingRitualEvent,
  type MeetingWrapCard,
} from "@/bindings";
import { Button } from "@/components/vg/button";
import { Checkbox } from "@/components/vg/checkbox";
import { consentFor } from "@/components/settings/meetings/MeetingStartGate";
import type { MeetingStartOptions } from "@/components/settings/meetings/meetingTypes";
import {
  followUpDraftText,
  meetingFollowUpDraft,
} from "@/components/settings/meetings/review/followUpDraft";
import { elapsedLabel } from "./elapsed";

/* The card IS the window: consent_panel.rs sizes the NSPanel to it and macOS
 * clips the panel to `--radius-panel` and draws its shadow, so nothing here
 * may grow past what that file measured and nothing here draws a shadow.
 * `glass-surface` is the app's frosted material; Rust writes `data-material`
 * on this webview only when a vibrancy view is actually behind it, so on a
 * solid window the card falls back to the raised surface. Its motion lives in
 * app/consent/consent-window.css under `consent-card`. */
const panelClass =
  "consent-card glass-surface flex h-full flex-col gap-2.5 rounded-panel border border-gray-alpha-400 bg-background-100 p-4 text-gray-1000";

/* The type layer (theme.css) in the roles this card uses: a 13/20 sentence-case
 * microlabel, a 13/18 heading, 13/20 body and a 12/16 note. Root type is 14px,
 * so the kit's rem sizes would land between the roles. */
const kickerClass = "text-[13px] leading-[20px] text-gray-900";
const titleClass = "min-w-0 truncate text-[13px] leading-[18px] font-semibold";
const bodyClass = "text-[13px] leading-[20px] text-gray-900";
const noteClass = "text-[12px] leading-[16px] text-gray-900";

type CardProps = {
  children: ReactNode;
  testId?: string;
  /* A card that asks for a decision is a dialog, named by its heading and
   * described by its body. The recording surfaces are status and pass
   * nothing. */
  labelledBy?: string;
  describedBy?: string;
  /* What Escape means on this card. Undefined on the recording surfaces: they
   * have no dismissal, only Stop. */
  onEscape?: () => void;
};

/* One surface for every state.
 *
 * The window is already on screen when the card mounts (consent_panel.rs
 * shows it, then the event lands here), so the card's entrance is what the
 * reader sees. Tab cycles inside the card and focus cannot leave it: this
 * document holds nothing else. */
function Card({
  children,
  testId,
  labelledBy,
  describedBy,
  onEscape,
}: CardProps) {
  return (
    <FocusScope.FocusScope
      asChild
      loop
      trapped
      /* React has already focused the card's `autoFocus` control in this
       * commit. Left alone, Radix would move focus to the first tabbable,
       * which on the prompt is a checkbox rather than Record. */
      onMountAutoFocus={(event) => event.preventDefault()}
    >
      <main
        className={panelClass}
        role={labelledBy === undefined ? undefined : "dialog"}
        aria-labelledby={labelledBy}
        aria-describedby={describedBy}
        data-testid={testId}
        onKeyDown={(event) => {
          if (event.key !== "Escape" || onEscape === undefined) return;
          event.preventDefault();
          onEscape();
        }}
      >
        {children}
      </main>
    </FocusScope.FocusScope>
  );
}

/* How long a cleared state's card stays mounted for its exit fade: the
 * `--duration-fast` consent-window.css runs it over, and the delay
 * consent_panel.rs waits before it hides the window. A state that arrives
 * inside that window takes the surface at once. */
const EXIT_MS = 120;

/* Returns the card to draw: the live one, or the last one while it fades. The
 * held card is the previous commit's element, so what fades is exactly what
 * was on screen. */
function useExitHold(card: ReactElement | null) {
  const latest = useRef<ReactElement | null>(null);
  const [expired, setExpired] = useState(false);
  const empty = card === null;
  useEffect(() => {
    if (card !== null) latest.current = card;
  });
  useEffect(() => {
    if (!empty) {
      setExpired(false);
      return;
    }
    const timer = window.setTimeout(() => setExpired(true), EXIT_MS);
    return () => window.clearTimeout(timer);
  }, [empty]);
  if (!empty) return { card, leaving: false };
  const held = expired ? null : latest.current;
  return { card: held, leaving: held !== null };
}

const startOptions = (prompt: DetectionPromptEvent): MeetingStartOptions => ({
  title:
    prompt.prompt.kind === "CalendarEvent"
      ? prompt.prompt.eventTitle
      : prompt.notificationTitle,
  origin: "suggestion",
  suggestionId: null,
  calendarEventKey:
    prompt.prompt.kind === "CalendarEvent" ? prompt.prompt.eventKey : null,
  sources: ["microphone", "system_audio"],
  degradedStartPolicy: "abort_if_required_source_fails",
  destination: { kind: "local" },
  preview: null,
});

type PrepCardProps = {
  card: MeetingPrepCard;
  onAction: (action: MeetingRitualAction) => void;
  t: TFunction;
  now?: number;
};

export function PrepCard({
  card,
  onAction,
  t,
  now = Date.now(),
}: PrepCardProps) {
  const minutes = Math.max(1, Math.ceil((card.startUtcMs - now) / 60_000));
  const participants = card.participants
    .map((participant) => {
      const meetings = t("meetings.prep.participantMeetings", {
        count: participant.meetingsCount,
      });
      return [participant.name, meetings, participant.organization]
        .filter(Boolean)
        .join(" · ");
    })
    .join("; ");

  return (
    <Card
      testId="prep-card"
      labelledBy="consent-title"
      describedBy="consent-body"
      onEscape={() => onAction("prep_dismiss")}
    >
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <p className={kickerClass}>{t("meetings.prep.label")}</p>
          <h1 id="consent-title" className={`truncate ${titleClass}`}>
            {t("meetings.prep.title", { title: card.title, count: minutes })}
          </h1>
        </div>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="-me-2 -mt-2 px-2 text-gray-800"
          aria-label={t("meetings.prep.dismiss")}
          onClick={() => onAction("prep_dismiss")}
        >
          ×
        </Button>
      </div>

      <p id="consent-body" className={`line-clamp-2 ${bodyClass}`}>
        <span className="font-medium text-gray-1000">
          {t("meetings.prep.lastTime")}
        </span>{" "}
        {card.headline}
      </p>

      {card.mineOpenLoopCount > 0 ? (
        <div className={`min-h-0 ${bodyClass}`}>
          <p className="font-medium">
            {t("meetings.prep.myOpenLoops", {
              count: card.mineOpenLoopCount,
            })}
          </p>
          <ul className="mt-0.5 space-y-0.5 text-gray-900">
            {card.mineOpenLoops.slice(0, 2).map((loop) => (
              <li className="truncate" key={loop}>
                · {loop}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      {card.waitingOnCount > 0 ? (
        <p className={bodyClass}>
          {t("meetings.prep.waitingOn", { count: card.waitingOnCount })}
        </p>
      ) : null}

      {participants !== "" ? (
        <p className={`line-clamp-2 ${noteClass}`}>
          <span className="font-medium text-gray-1000">
            {t("meetings.prep.participants")}
          </span>{" "}
          {participants}
        </p>
      ) : null}

      {/* The primary takes focus when the card appears: Enter records, Escape
       * dismisses. Open brief stands in when nothing can be armed. */}
      <div className="mt-auto flex justify-end gap-2">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          autoFocus={!card.canRecordWhenStarts}
          onClick={() => onAction("prep_open_brief")}
        >
          {t("meetings.prep.openBrief")}
        </Button>
        {card.canRecordWhenStarts ? (
          <Button
            type="button"
            size="sm"
            autoFocus
            onClick={() => onAction("prep_record_when_starts")}
          >
            {t("meetings.prep.recordWhenStarts")}
          </Button>
        ) : null}
      </div>
    </Card>
  );
}

type WrapCardProps = {
  card: MeetingWrapCard;
  copied: boolean;
  onAction: (action: MeetingRitualAction) => void;
  onCopy: () => void;
  t: TFunction;
};

export function WrapCard({ card, copied, onAction, onCopy, t }: WrapCardProps) {
  const waiting =
    card.waitingOnCount === 0
      ? null
      : card.waitingOnNames.length === 1
        ? t("consentPanel.wrap.waitingOnPerson", {
            count: card.waitingOnCount,
            name: card.waitingOnNames[0],
          })
        : t("consentPanel.wrap.waitingOn", { count: card.waitingOnCount });
  const unresolvedSpeakers =
    card.unresolvedSpeakerCount !== null && card.unresolvedSpeakerCount > 0
      ? t("consentPanel.wrap.unresolvedSpeakers", {
          count: card.unresolvedSpeakerCount,
        })
      : null;
  /* Every count this card carries reads on one line, so a third count has
   * one home rather than a second one beside the title. */
  const delta = [
    card.followUpCount > 0
      ? t("consentPanel.wrap.followUps", { count: card.followUpCount })
      : null,
    waiting,
    unresolvedSpeakers,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <Card
      testId="wrap-card"
      labelledBy="consent-title"
      describedBy="consent-body"
      onEscape={() => onAction("wrap_done")}
    >
      <div>
        <p className={kickerClass}>{t("consentPanel.wrap.label")}</p>
        <h1 id="consent-title" className={`truncate ${titleClass}`}>
          {t("consentPanel.wrap.title", { title: card.title })}
        </h1>
      </div>
      <p id="consent-body" className={`line-clamp-2 ${bodyClass}`}>
        {card.headline}
      </p>
      {delta !== "" ? <p className={bodyClass}>{delta}</p> : null}
      <div className="mt-auto flex justify-end gap-2">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => onAction("wrap_open_notes")}
        >
          {t("consentPanel.wrap.openNotes")}
        </Button>
        <Button type="button" variant="outline" size="sm" onClick={onCopy}>
          {copied
            ? t("consentPanel.wrap.copied")
            : t("consentPanel.wrap.copyFollowUp")}
        </Button>
        <Button
          type="button"
          size="sm"
          autoFocus
          onClick={() => onAction("wrap_done")}
        >
          {t("consentPanel.wrap.done")}
        </Button>
      </div>
      <span className="sr-only" role="status" aria-live="polite">
        {copied ? t("consentPanel.wrap.copied") : ""}
      </span>
    </Card>
  );
}

type RecordingCardProps = {
  card: MeetingRecordingCard;
  onAction: (action: MeetingRitualAction) => void;
  t: TFunction;
  now: number;
};

/* What an auto-started call looks like while it runs.
 *
 * The two actions are the whole point of the card: an operator who did not ask
 * for this recording out loud can end it, and can end the standing grant that
 * produced it, without going to Settings first. */
export function RecordingCard({ card, onAction, t, now }: RecordingCardProps) {
  return (
    <Card testId="recording-card">
      <div className="flex items-center gap-2">
        <span className="size-2 rounded-full bg-red-700" aria-hidden="true" />
        <strong className="text-[13px] leading-[20px] font-semibold">
          {t("consentPanel.recordingStarted")}
        </strong>
        <span className={`ms-auto tabular-nums ${bodyClass}`}>
          {elapsedLabel(card.startedAtUtcMs, now)}
        </span>
      </div>
      <p className="truncate text-[14px] leading-[20px] font-medium">
        {card.appName}
      </p>
      <div className="mt-auto flex items-center justify-end gap-2">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => onAction("recording_forget_app")}
        >
          {t("consentPanel.forgetApp")}
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => onAction("recording_stop")}
        >
          {t("consentPanel.stop")}
        </Button>
      </div>
    </Card>
  );
}

export default function ConsentPanel() {
  const { t } = useTranslation();
  const [prompt, setPrompt] = useState<DetectionPromptEvent | null>(null);
  const [ritual, setRitual] = useState<MeetingRitualEvent | null>(null);
  const [status, setStatus] = useState<DetectionStatus | null>(null);
  const [active, setActive] = useState<MeetingConsentPanelSessionState | null>(
    null,
  );
  const [alwaysRecord, setAlwaysRecord] = useState(false);
  const [announceInChat, setAnnounceInChat] = useState(false);
  const [starting, setStarting] = useState(false);
  const [copied, setCopied] = useState(false);
  const [now, setNow] = useState(Date.now());
  const activeRef = useRef<MeetingConsentPanelSessionState | null | undefined>(
    undefined,
  );

  const refreshActive = useCallback(async () => {
    const result = await commands.meetingConsentPanelActiveState();
    if (result.status === "error") {
      console.error("Could not refresh the active meeting", result.error);
      return;
    }
    activeRef.current = result.data;
    setActive(result.data);
    if (result.data !== null) {
      setPrompt(null);
      /* The recording card is *about* the active session, so an active session
       * is not a reason to drop it. A *different* active session is: the card's
       * capture ended and the operator started another, and Stop on the stale
       * card would name a capture that is gone. Its own retraction event ends
       * it otherwise. */
      const activeSessionId = result.data.snapshot.session_id;
      setRitual((current) =>
        current?.ritual.kind === "recording" &&
        current.ritual.card.sessionId === activeSessionId
          ? current
          : null,
      );
    }
  }, []);

  /* The disclosure is posted from here because the words come from the i18next
   * catalog and the backend cannot reach it. The backend decides whether one is
   * owed and records what the paste did, so this fires once per recording even
   * though the live state is re-read on every change to the meeting. */
  useEffect(() => {
    if (active?.disclosure.kind !== "pending") return;
    const sessionId = active.snapshot.session_id;
    const line = t("consentPanel.announceLine", {
      name: active.disclosure.notetaker,
    });
    void commands
      .meetingAnnounceDisclosure(sessionId, line)
      .then(() => refreshActive())
      .catch((error) => {
        console.error("Could not announce the recording", error);
      });
  }, [active, refreshActive, t]);

  useEffect(() => {
    void commands
      .detectionStatusGet()
      .then(setStatus)
      .catch((error) => {
        console.error("Could not refresh meeting detection", error);
      });
    void refreshActive();
    const listeners = Promise.all([
      events.detectionPrompt.listen((event) => {
        if (event.payload.delivery !== "panel") {
          setPrompt((current) =>
            current?.promptId === event.payload.promptId ? null : current,
          );
          return;
        }
        if (activeRef.current !== null && activeRef.current !== undefined)
          return;
        setRitual(null);
        setPrompt(event.payload);
        setAlwaysRecord(false);
        setAnnounceInChat(event.payload.announceInChat);
        void commands
          .detectionPromptPanelAck(event.payload.promptId)
          .catch((error) => {
            console.error("Could not acknowledge the consent panel", error);
          });
      }),
      events.detectionPromptRetracted.listen((event) =>
        setPrompt((current) =>
          current?.promptId === event.payload.promptId ? null : current,
        ),
      ),
      events.meetingRitual.listen((event) => {
        if (event.payload.delivery !== "panel") {
          setRitual((current) =>
            current?.ritualId === event.payload.ritualId ? null : current,
          );
          return;
        }
        /* Same exception as `refreshActive`: the recording card names the
         * capture that is running, so it is delivered during one by design. */
        if (
          event.payload.ritual.kind !== "recording" &&
          activeRef.current !== null &&
          activeRef.current !== undefined
        )
          return;
        setPrompt(null);
        setCopied(false);
        setRitual(event.payload);
        void commands
          .meetingRitualPanelAck(event.payload.ritualId)
          .catch((error) => {
            console.error("Could not acknowledge the meeting ritual", error);
          });
      }),
      events.meetingRitualRetracted.listen((event) =>
        setRitual((current) =>
          current?.ritualId === event.payload.ritualId ? null : current,
        ),
      ),
      events.detectionStatus.listen((event) => setStatus(event.payload)),
      events.meetingSessionChanged.listen(() => void refreshActive()),
    ]).catch((error) => {
      console.error("Could not subscribe to consent panel events", error);
      return [];
    });
    return () => {
      void listeners.then((stops) => stops.forEach((stop) => stop()));
    };
  }, [refreshActive]);

  useEffect(() => {
    if (
      active === null &&
      ritual?.ritual.kind !== "prep" &&
      ritual?.ritual.kind !== "recording"
    )
      return;
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, [active, ritual]);

  /* Whether the recording card owes the refused-disclosure row. Derived here
   * because the window has to grow for it before the row paints. */
  const disclosureRefused =
    active?.disclosure.kind === "attempted" &&
    active.disclosure.receipt.outcome === "definitely_not_dispatched";

  /* The panel's window was sized for a recording card without that row. Only
   * this document knows the row is coming, so it asks for the height; the
   * request is idempotent and the backend re-pins the top-right corner. */
  useEffect(() => {
    if (active === null) return;
    void commands
      .meetingConsentPanelFitDisclosure(disclosureRefused)
      .catch((error) => {
        console.error("Could not size the recording card", error);
      });
  }, [active, disclosureRefused]);

  const briefing = useMemo(() => {
    if (prompt?.prompt.kind !== "CalendarEvent") return null;
    const countdown = status?.countdown;
    if (countdown?.event.eventKey !== prompt.prompt.eventKey) return null;
    const person = countdown.briefing[0];
    if (person === undefined) return null;
    const loops = countdown.briefing.reduce(
      (total, row) => total + row.open_loops.length,
      0,
    );
    return t("consentPanel.seriesBrief", {
      nth: person.meetings_count + 1,
      person: person.display_name,
      count: loops,
    });
  }, [prompt, status, t]);

  const title = useMemo(() => {
    if (prompt === null) return "";
    switch (prompt.prompt.kind) {
      case "AppCall":
        return t("consentPanel.appCallTitle", { app: prompt.prompt.appName });
      case "CalendarEvent":
        return t("consentPanel.calendarTitle", {
          title: prompt.prompt.eventTitle,
        });
      case "AppMeeting":
      case "AppHuddle":
      case "BrowserCall":
        return t("consentPanel.appTitle", { app: prompt.prompt.appName });
      case "UnknownMicSource":
        return t("consentPanel.genericTitle");
    }
  }, [prompt, t]);

  const ritualAction = async (action: MeetingRitualAction) => {
    if (ritual === null) return;
    const result = await commands.meetingRitualRespond(ritual.ritualId, action);
    if (
      result.status === "ok" &&
      result.data &&
      action !== "wrap_follow_up_copied"
    ) {
      setRitual(null);
    }
  };

  const copyFollowUp = async () => {
    if (ritual?.ritual.kind !== "wrap") return;
    try {
      const draft = await meetingFollowUpDraft(
        crypto.randomUUID(),
        ritual.ritual.card.sessionId,
      );
      await navigator.clipboard.writeText(followUpDraftText(draft, t));
      setCopied(true);
      await ritualAction("wrap_follow_up_copied");
    } catch (error: unknown) {
      console.error("Could not copy the meeting follow-up", error);
    }
  };

  const record = async () => {
    if (prompt === null) return;
    setStarting(true);
    const options = startOptions(prompt);
    try {
      const result = await commands.meetingConsentPanelStart({
        prompt_id: prompt.promptId,
        operation_id: crypto.randomUUID(),
        consent: consentFor(options, [], false),
        always_record_series: alwaysRecord,
        announce_in_chat: announceInChat,
      });
      if (result.status === "error") {
        console.error("Could not start the meeting", result.error);
        setPrompt(null);
        return;
      }
      if (result.data.snapshot.phase === "capturing_recording")
        await refreshActive();
      setPrompt(null);
    } catch (error: unknown) {
      console.error("Could not start the meeting", error);
      setPrompt(null);
    } finally {
      setStarting(false);
    }
  };

  const ignore = async () => {
    if (prompt === null) return;
    const promptId = prompt.promptId;
    setPrompt(null);
    try {
      await commands.detectionPromptRespond(promptId, false);
    } catch (error: unknown) {
      console.error("Could not dismiss the detection prompt", error);
    }
  };

  const stop = async () => {
    if (active === null) return;
    const result = await commands.meetingStop(
      {
        operation_id: crypto.randomUUID(),
        session_id: active.snapshot.session_id,
        expected_revision: active.snapshot.revision,
      },
      "consent_panel",
    );
    if (result.status === "error") {
      console.error("Could not stop the meeting", result.error);
      return;
    }
    await refreshActive();
  };

  const forgetSeries = async () => {
    if (active === null) return;
    const result = await commands.meetingConsentPanelForgetSeries(
      active.snapshot.session_id,
    );
    if (result.status === "error") {
      console.error("Could not forget the standing consent", result.error);
      return;
    }
    setActive({ ...active, standing_series_key: null });
  };

  /* Each state mounts its own card, keyed by what it is about, so a new prompt
   * or ritual gets the entrance again and a replaced one never inherits stale
   * motion. The recording card comes ahead of the generic pill: it names the
   * app whose standing grant started the capture, and offers to take that
   * grant back. The pill knows neither. */
  const card = ((): ReactElement | null => {
    if (ritual?.ritual.kind === "recording") {
      return (
        <RecordingCard
          key={`ritual:${ritual.ritualId}`}
          card={ritual.ritual.card}
          onAction={(action) => void ritualAction(action)}
          t={t}
          now={now}
        />
      );
    }

    if (active !== null) {
      return (
        <Card key={`active:${active.snapshot.session_id}`}>
          <div className="flex items-center gap-2">
            <span
              className="size-2 rounded-full bg-red-700"
              aria-hidden="true"
            />
            <strong className="text-[13px] leading-[18px] font-semibold">
              {t("consentPanel.recording")}
            </strong>
            <span className={`ms-auto tabular-nums ${noteClass}`}>
              {elapsedLabel(active.snapshot.started_at_utc_ms, now)}
            </span>
          </div>
          <p className="truncate text-[13px] leading-[18px] font-medium">
            {active.snapshot.title}
          </p>
          {/* A disclosure the target would not take is worth saying once,
           * quietly: the recording is running either way, and the room was
           * not told. The window grew for this row before it drew, in the
           * effect above. */}
          {disclosureRefused ? (
            <p className={noteClass} data-slot="consent-announce-refused">
              {t("consentPanel.announceRefused")}
            </p>
          ) : null}
          <div className="mt-auto flex items-center justify-end gap-2">
            {active.standing_series_key !== null ? (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={forgetSeries}
              >
                {t("consentPanel.forgetSeries")}
              </Button>
            ) : null}
            <Button type="button" variant="outline" size="sm" onClick={stop}>
              {t("consentPanel.stop")}
            </Button>
          </div>
        </Card>
      );
    }

    if (prompt !== null) {
      const calendarPrompt = prompt.prompt.kind === "CalendarEvent";
      return (
        <Card
          key={`prompt:${prompt.promptId}`}
          labelledBy="consent-title"
          describedBy="consent-body"
          onEscape={() => void ignore()}
        >
          {/* One title row. The glyph says what the card is about at a glance
           * and costs no height; the title carries the meeting or app name and
           * truncates, with the full text on hover for a long calendar
           * title. */}
          <div className="flex min-w-0 items-center gap-2">
            <Mic
              aria-hidden="true"
              className="size-3.5 shrink-0 text-gray-900"
            />
            <h1 id="consent-title" className={titleClass} title={title}>
              {title}
            </h1>
          </div>
          {briefing !== null ? (
            <p className={`${noteClass} truncate`} title={briefing}>
              {briefing}
            </p>
          ) : null}
          {/* The one line of prose. On the first prompt it is the friendlier
           * introduction; after that the plain fact. Two sentences saying the
           * same thing is what made this card tall. */}
          <p id="consent-body" className={noteClass}>
            {prompt.showIntroduction
              ? t("consentPanel.introduction")
              : t("consentPanel.assurance")}
          </p>
          {calendarPrompt ? (
            <div className="flex flex-col gap-1.5">
              <label className="flex items-center gap-2 text-[12px] leading-[16px]">
                <Checkbox
                  className="border-gray-700"
                  checked={alwaysRecord}
                  onCheckedChange={(checked) =>
                    setAlwaysRecord(checked === true)
                  }
                />
                <span>{t("consentPanel.alwaysRecord")}</span>
              </label>
              {/* Remembered per series, like the decision above it. */}
              <label
                className="flex items-center gap-2 text-[12px] leading-[16px]"
                data-slot="consent-announce"
              >
                <Checkbox
                  className="border-gray-700"
                  checked={announceInChat}
                  onCheckedChange={(checked) =>
                    setAnnounceInChat(checked === true)
                  }
                />
                <span>{t("consentPanel.announceInChat")}</span>
              </label>
            </div>
          ) : null}
          <div className="mt-auto flex justify-end gap-2">
            <Button type="button" variant="ghost" size="sm" onClick={ignore}>
              {t("consentPanel.ignore")}
            </Button>
            {/* Focus lands here: Enter records, Escape ignores. */}
            <Button
              type="button"
              size="sm"
              autoFocus
              disabled={starting}
              onClick={record}
            >
              {t("consentPanel.record")}
            </Button>
          </div>
        </Card>
      );
    }

    if (ritual?.ritual.kind === "prep") {
      return (
        <PrepCard
          key={`ritual:${ritual.ritualId}`}
          card={ritual.ritual.card}
          onAction={(action) => void ritualAction(action)}
          t={t}
          now={now}
        />
      );
    }
    if (ritual?.ritual.kind === "wrap") {
      return (
        <WrapCard
          key={`ritual:${ritual.ritualId}`}
          card={ritual.ritual.card}
          copied={copied}
          onAction={(action) => void ritualAction(action)}
          onCopy={() => void copyFollowUp()}
          t={t}
        />
      );
    }
    return null;
  })();

  const shown = useExitHold(card);
  /* One box that never remounts, so the held card keeps its node and fades
   * rather than entering again. `contents` keeps it out of layout. */
  return (
    <div className="contents" data-leaving={shown.leaving ? "" : undefined}>
      {shown.card}
    </div>
  );
}
