import React, { useId, useState } from "react";
import { Check, ChevronDown, CircleDashed, Clock, X } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { TFunction } from "i18next";
import { useTranslation } from "react-i18next";
import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";
import type {
  MeetingSuggestion,
  PersonBriefingRow,
  SourceKind,
} from "@/bindings";
import { cn } from "@/lib/cn";
import { formatDurationShort, formatEntryTimestamp } from "@/lib/utils/format";
import { Microlabel, SETTINGS_SURFACE } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Switch } from "@/components/vg/switch";
import { MeetingSourceChip } from "./MeetingSourceChip";
import { PreMeetingBriefing } from "@/components/people/PreMeetingBriefing";
import type { MeetingNotesTemplate } from "./meetingAnalytics";
import { MEETING_SOURCES, meetingProviderKey, sourceKey } from "./meetingUtils";
import type {
  CalendarEventSummary,
  NotificationAccess,
  ParticipationStatus,
} from "./detectionStore";

/* The one shape a meeting takes before it is recorded.
 *
 * Three surfaces show a meeting that has not started: the countdown for a
 * calendar event, an offer raised by a running meeting app, and the preflight
 * that previews a start already configured. They used to be three different
 * rows saying three different amounts, which meant the operator learned the
 * layout three times and still could not tell what Sona knew about the call.
 *
 * So one card, and a hard rule about what goes in it: a row exists only when
 * the backend supplied its value. An event with no attendee list has no
 * PARTICIPANTS row, not an empty one; an offer from a running app has no TIME
 * row, because nothing scheduled it. Absence is information, and faking a row
 * to keep the shape tidy would spend the operator's trust on symmetry.
 *
 * Every measured value on the card is presented KEY then VALUE, so the facts
 * read as facts and the one line of prose on the surface stays the one line
 * of prose. */

/** The facts about one meeting-to-be. Pure data: every control the card can
 * drive is passed separately, because the facts travel with a start request
 * and the controls belong to whichever surface is rendering. */
export interface MeetingPreviewFacts {
  /** Stable identity on its surface: event key, prompt id, or offer id. */
  id: string;
  title: string;
  /** Which signal produced this preview, which is also its icon. */
  origin: "calendar" | "app";
  startUtcMs: number | null;
  endUtcMs: number | null;
  /** Title of the calendar the event sits on. */
  calendarName: string | null;
  /** The meeting application detection named, when it named one. */
  appName: string | null;
  /** Participants the event carries, including any EventKit would not name. */
  attendeeCount: number | null;
  participants: MeetingPreviewParticipant[];
  /** The event's own notes. */
  description: string | null;
  /** The URL attached to the event, which for a scheduled call is the join
   * link. */
  url: string | null;
}

export interface MeetingPreviewParticipant {
  name: string;
  status: ParticipationStatus;
  isSelf: boolean;
}

/** How a prompt for this meeting reaches the operator, and whether reaching
 * its start with this card open opens the capture by itself. */
export interface MeetingPreviewNotify {
  access: NotificationAccess;
  /** Whether this particular prompt was delivered natively. `null` when no
   * prompt was raised for this preview. */
  delivered: boolean | null;
  autoOpen: {
    checked: boolean;
    onChange: (next: boolean) => void;
    disabled: boolean;
  } | null;
}

/** The capture sources the next press will request. */
export interface MeetingPreviewRecording {
  armed: SourceKind[];
  /** Absent once the sources are settled: the preflight cannot re-arm them. */
  onToggle?: (source: SourceKind) => void;
  disabled?: boolean;
}

export interface MeetingPreviewCardProps {
  facts: MeetingPreviewFacts;
  /** Live seconds until the event starts, when a countdown is running. */
  secondsToStart?: number | null;
  /** Deterministic relationship context attached to a calendar countdown. */
  briefing?: PersonBriefingRow[];
  notify?: MeetingPreviewNotify | null;
  recording?: MeetingPreviewRecording | null;
  /** The shape generated notes will take, read from the real setting. */
  notesTemplate?: MeetingNotesTemplate | null;
  /** Whether that shape came from this series' own remembered choice rather
   * than the app default. The row says so, because "One-to-one" with no
   * provenance reads as a global setting the reader did not make. */
  notesTemplateFromSeries?: boolean;
  defaultExpanded?: boolean;
  /** Routes through the surface's existing start path, consent screen and
   * all. `null` on a surface that has no start to offer. */
  onStart?: (() => void) | null;
  starting?: boolean;
  onSkip?: (() => void) | null;
}

const PARTICIPATION_ICON = {
  accepted: Check,
  declined: X,
  tentative: CircleDashed,
  pending: Clock,
  unknown: null,
} as const satisfies Record<ParticipationStatus, LucideIcon | null>;

/* Ordered by how much the answer tells you, so the tally reads down from the
 * people who are coming to the people who never said. */
const PARTICIPATION_ORDER: ParticipationStatus[] = [
  "accepted",
  "declined",
  "tentative",
  "pending",
  "unknown",
];

/** Host for a link row, falling back to the whole string for schemes that
 * carry no host. A calendar URL is whatever the organizer pasted. */
const linkLabel = (url: string): string => {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
};

const openLink = async (url: string, failure: string) => {
  try {
    await openUrl(url);
  } catch (error) {
    console.error("Failed to open the meeting link:", error);
    toast.error(failure);
  }
};

/**
 * A labelled list of preview cards: one surface, rows divided by hairlines,
 * the section's name above it.
 *
 * Round 6 collapsed the old card-per-offer stack into this. A page that
 * offered two meetings drew two bordered boxes inside a list inside a page,
 * which is the nesting the grammar exists to prevent — and with one offer, the
 * commonest case, the box was a border drawn around a single row.
 */
export const MeetingPreviewList: React.FC<{
  label: string;
  children: React.ReactNode;
}> = ({ label, children }) => (
  <section className="flex flex-col gap-2">
    <h2 className="min-h-5">
      <Microlabel>{label}</Microlabel>
    </h2>
    <ul aria-label={label} className={SETTINGS_SURFACE}>
      {children}
    </ul>
  </section>
);

interface PreviewRowProps {
  label: string;
  children: React.ReactNode;
}

/* KEY then VALUE, both as type. The row used to lead with a glyph of its own
 * label — a bell beside "Notify", a microphone beside "Recording" — which is
 * the same word twice, once in a form nobody can read. */
const PreviewRow: React.FC<PreviewRowProps> = ({ label, children }) => (
  <li className="flex items-baseline justify-between gap-6 px-6 py-3">
    <Microlabel className="flex-none">{label}</Microlabel>
    <span className="flex min-w-0 flex-wrap items-center justify-end gap-x-2 gap-y-1 text-[14px] leading-[21px] text-gray-1000">
      {children}
    </span>
  </li>
);

export const MeetingPreviewCard: React.FC<MeetingPreviewCardProps> = ({
  facts,
  secondsToStart = null,
  briefing = [],
  notify = null,
  recording = null,
  notesTemplate = null,
  notesTemplateFromSeries = false,
  defaultExpanded = false,
  onStart = null,
  starting = false,
  onSkip = null,
}) => {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(defaultExpanded);
  const [descriptionOpen, setDescriptionOpen] = useState(false);
  const bodyId = useId();

  const durationSeconds =
    facts.startUtcMs === null || facts.endUtcMs === null
      ? null
      : Math.max(0, (facts.endUtcMs - facts.startUtcMs) / 1000);

  const named = facts.participants;
  const unnamed =
    facts.attendeeCount === null
      ? 0
      : Math.max(0, facts.attendeeCount - named.length);
  const tally = PARTICIPATION_ORDER.map((status) => ({
    status,
    count: named.filter((person) => person.status === status).length,
  })).filter((entry) => entry.count > 0);

  /* What the head row can measure.
   *
   * The head is the card's summary, so it states only what the body is not
   * stating two rows below it. Open, the start, the duration and the head
   * count are all rows of their own, and printing them twice crowded the
   * title into an ellipsis to make room for its own echo. The countdown is
   * the exception: no row carries it, and it is the reason the card is on
   * screen at all.
   *
   * An offer from a running app measures nothing, so it carries no chips and
   * no empty rail where chips would have gone. */
  const summarised = !expanded;
  const chips: [string, string][] = [];
  if (summarised && facts.startUtcMs !== null) {
    chips.push(["start", formatEntryTimestamp(facts.startUtcMs)]);
  }
  if (summarised && durationSeconds !== null) {
    chips.push(["duration", formatDurationShort(durationSeconds)]);
  }
  if (secondsToStart !== null) {
    chips.push([
      "countdown",
      t("meetings.detection.pane.countdown", "Starts in {{seconds}}s", {
        seconds: Math.max(0, secondsToStart),
      }),
    ]);
  }
  /* The participants row only exists for an event that named someone, so an
   * attendee count with no names behind it stays in the head either way. */
  if (
    facts.attendeeCount !== null &&
    facts.attendeeCount !== 0 &&
    (summarised || named.length === 0)
  ) {
    chips.push([
      "attendees",
      /* The suffix is picked here, not by i18next: a plural category i18next
       * resolves (few, many) has no key in any locale file, and every locale
       * carries exactly _one and _other. Same shape as SecureInputWarning. */
      t(
        `meetings.preview.attendees_${
          facts.attendeeCount === 1 ? "one" : "other"
        }`,
        { count: facts.attendeeCount },
      ),
    ]);
  }

  /* The decision lives in the head row, not a footer: it must never sit
   * behind the disclosure, and a footer band under a collapsed card is a
   * reserved blank the operator pays for on every card. */
  const actions =
    onStart === null && onSkip === null ? null : (
      <div
        data-slot="preview-actions"
        className="flex flex-none items-center gap-1.5"
      >
        {onSkip === null ? null : (
          <Button type="button" variant="outline" size="sm" onClick={onSkip}>
            {t("meetings.preview.actions.skip", "Skip")}
          </Button>
        )}
        {onStart === null ? null : (
          <Button type="button" size="sm" onClick={onStart} disabled={starting}>
            {starting
              ? t("meetings.start.starting", "Starting…")
              : t("meetings.start.action", "Start recording")}
          </Button>
        )}
      </div>
    );

  /* Facts can arrive with an empty title (an untitled calendar event, a
   * foreign payload). The header never renders blank: name the origin
   * honestly instead. */
  const title =
    facts.title.trim() ||
    facts.appName ||
    (facts.origin === "calendar"
      ? t("meetings.preview.untitled.calendar", "Calendar event")
      : t("meetings.preview.untitled.app", "Microphone in use"));

  return (
    <li data-slot="meeting-preview">
      <div
        data-slot="preview-head"
        className="flex items-center gap-3 px-6 py-3.5"
      >
        <button
          type="button"
          data-slot="preview-summary"
          aria-expanded={expanded}
          aria-controls={bodyId}
          onClick={() => setExpanded(!expanded)}
          className="flex min-w-0 flex-1 items-center gap-3 rounded-md text-start"
        >
          <span
            data-slot="preview-title"
            className="truncate text-[14px] leading-[21px] font-medium text-gray-1000"
          >
            {title}
          </span>
          {chips.length === 0 ? null : (
            <span
              data-slot="preview-facts"
              className="flex flex-wrap items-baseline gap-x-3"
            >
              {chips.map(([key, value]) => (
                <Microlabel key={key} className="tabular-nums text-gray-900">
                  {value}
                </Microlabel>
              ))}
            </span>
          )}
          <ChevronDown
            aria-hidden="true"
            className={cn(
              "ms-auto size-3.5 flex-none text-gray-700 transition-transform",
              expanded && "rotate-180",
            )}
          />
        </button>
        {actions}
      </div>

      <PreMeetingBriefing rows={briefing} />

      {/* The collapse is a grid track, so the rows stay in the document and
       * have something to animate out of. `visibility` is what takes them out
       * of the tab order while they are closed. */}
      <div
        id={bodyId}
        data-slot="preview-body"
        data-open={expanded}
        className="group/body grid grid-rows-[0fr] transition-[grid-template-rows] duration-150 ease-out data-[open=true]:grid-rows-[1fr]"
      >
        <div className="invisible overflow-hidden group-data-[open=true]/body:visible">
          <ul className="divide-y divide-gray-alpha-400 border-t border-gray-alpha-400">
            {facts.startUtcMs === null ? null : (
              <PreviewRow label={t("meetings.preview.rows.time", "Time")}>
                <span className="tabular-nums">
                  {formatEntryTimestamp(facts.startUtcMs)}
                </span>
                {durationSeconds === null ? null : (
                  <Microlabel className="tabular-nums text-gray-900">
                    {formatDurationShort(durationSeconds)}
                  </Microlabel>
                )}
              </PreviewRow>
            )}

            {facts.calendarName === null ? null : (
              <PreviewRow
                label={t("meetings.preview.rows.calendar", "Calendar")}
              >
                {facts.calendarName}
              </PreviewRow>
            )}

            {facts.appName === null ? null : (
              <PreviewRow label={t("meetings.preview.rows.app", "App")}>
                {facts.appName}
              </PreviewRow>
            )}

            {notify === null ? null : (
              <PreviewRow label={t("meetings.preview.rows.notify", "Notify")}>
                <span
                  className={cn(
                    "text-[13px] leading-[18px]",
                    notify.access === "authorized"
                      ? "text-gray-900"
                      : "text-amber-900",
                  )}
                >
                  {notify.delivered === true
                    ? t(
                        "meetings.preview.notify.delivered",
                        "Notification sent",
                      )
                    : notify.access === "authorized"
                      ? t(
                          "meetings.preview.notify.willNotify",
                          "Notifies you at the start",
                        )
                      : t(
                          "meetings.preview.notify.inApp",
                          "Shown in Sona only: notifications are off",
                        )}
                </span>
                {notify.autoOpen === null ? null : (
                  <Switch
                    checked={notify.autoOpen.checked}
                    disabled={notify.autoOpen.disabled}
                    onCheckedChange={notify.autoOpen.onChange}
                    aria-label={t(
                      "meetings.preview.notify.autoOpen",
                      "Open this meeting when it starts",
                    )}
                  />
                )}
              </PreviewRow>
            )}

            {recording === null ? null : (
              <PreviewRow
                label={t("meetings.preview.rows.recording", "Recording")}
              >
                {recording.onToggle === undefined ? (
                  recording.armed.length === 0 ? (
                    <span className="text-[13px] leading-[18px] text-amber-900">
                      {t("meetings.preview.recording.none", "No source chosen")}
                    </span>
                  ) : (
                    recording.armed
                      .map((source) => t(sourceKey(source)))
                      .join(" · ")
                  )
                ) : (
                  MEETING_SOURCES.map((source) => (
                    <MeetingSourceChip
                      key={source}
                      source={source}
                      selected={recording.armed.includes(source)}
                      disabled={recording.disabled === true}
                      onToggle={() => recording.onToggle?.(source)}
                    />
                  ))
                )}
              </PreviewRow>
            )}

            {notesTemplate === null ? null : (
              <PreviewRow label={t("meetings.preview.rows.notes", "Notes")}>
                {t(`meetings.notes.templates.${notesTemplate}`)}
                {notesTemplateFromSeries ? (
                  <Microlabel>
                    {t("meetings.preview.rows.notesSeries", "for this series")}
                  </Microlabel>
                ) : null}
              </PreviewRow>
            )}

            {named.length === 0 ? null : (
              <PreviewRow
                label={t("meetings.preview.rows.participants", "Participants")}
              >
                <span className="flex flex-wrap items-center justify-end gap-x-2 gap-y-1">
                  {tally.map(({ status, count }) => (
                    <Microlabel key={status}>
                      {t("meetings.preview.participation.tally", {
                        label: t(`meetings.preview.participation.${status}`),
                        count,
                        defaultValue: "{{label}} {{count}}",
                      })}
                    </Microlabel>
                  ))}
                </span>
                <span className="flex flex-wrap items-center justify-end gap-1">
                  {named.map((person) => {
                    const Glyph = PARTICIPATION_ICON[person.status];
                    const status = t(
                      `meetings.preview.participation.${person.status}`,
                    );
                    return (
                      <span
                        key={`${person.name}:${person.status}`}
                        data-slot="preview-person"
                        data-status={person.status}
                        title={`${person.name} — ${status}`}
                        className="inline-flex items-center gap-1 rounded-md border border-gray-alpha-400 px-1.5 text-[13px] leading-[18px] text-gray-900 data-[status=declined]:text-gray-700"
                      >
                        {Glyph === null ? null : (
                          <Glyph aria-hidden="true" className="size-3" />
                        )}
                        {person.name}
                        {person.isSelf ? (
                          <Microlabel>
                            {t("meetings.preview.participation.you", "You")}
                          </Microlabel>
                        ) : null}
                      </span>
                    );
                  })}
                  {unnamed === 0 ? null : (
                    <Microlabel>
                      {t(
                        `meetings.preview.participation.unnamed_${
                          unnamed === 1 ? "one" : "other"
                        }`,
                        { count: unnamed },
                      )}
                    </Microlabel>
                  )}
                </span>
              </PreviewRow>
            )}

            {facts.url === null ? null : (
              <PreviewRow label={t("meetings.preview.rows.link", "Link")}>
                <button
                  type="button"
                  className="truncate rounded-md text-accent-strong hover:underline"
                  onClick={() =>
                    void openLink(
                      facts.url ?? "",
                      t(
                        "meetings.preview.linkFailed",
                        "Sona could not open that link.",
                      ),
                    )
                  }
                >
                  {linkLabel(facts.url)}
                </button>
              </PreviewRow>
            )}

            {facts.description === null ? null : (
              <PreviewRow
                label={t("meetings.preview.rows.description", "Description")}
              >
                <span
                  data-slot="preview-description"
                  data-open={descriptionOpen}
                  className="min-w-0 text-pretty line-clamp-1 data-[open=true]:line-clamp-none"
                >
                  {facts.description}
                </span>
                <button
                  type="button"
                  aria-expanded={descriptionOpen}
                  onClick={() => setDescriptionOpen(!descriptionOpen)}
                  /* Inline text actions on this card carry the one accent and
                   * underline on hover — the same affordance as the link row.
                   * A grey status word beside prose would read as more prose. */
                  className="flex-none rounded-md text-[13px] text-accent-strong hover:underline"
                >
                  {descriptionOpen
                    ? t("meetings.preview.description.less", "Less")
                    : t("meetings.preview.description.more", "More")}
                </button>
              </PreviewRow>
            )}
          </ul>
        </div>
      </div>
    </li>
  );
};

/** The facts a calendar event supplies. Everything the calendar left empty
 * stays null here, so the card omits that row rather than inventing it. The
 * title is the exception — the header may never be blank — so an untitled
 * event is named for what it is. */
export const eventFacts = (
  event: CalendarEventSummary,
  t: TFunction,
): MeetingPreviewFacts => ({
  id: event.eventKey,
  title:
    event.title.trim() ||
    t("meetings.preview.untitled.calendar", "Calendar event"),
  origin: "calendar",
  startUtcMs: event.startUtcMs,
  endUtcMs: event.endUtcMs,
  calendarName: event.calendarName,
  /* Detection ties no application to a calendar event: the event is evidence
   * on its own. An APP row here would be a guess. */
  appName: null,
  attendeeCount: event.attendeeCount,
  participants: event.attendees.map((attendee) => ({
    name: attendee.name,
    status: attendee.status,
    isSelf: attendee.isSelf,
  })),
  description: event.notes,
  url: event.url,
});

/** The facts an offer from a running meeting app supplies, which is the app
 * and nothing else: the suggestion payload is content-free by design, so this
 * card is deliberately short. */
export const suggestionFacts = (
  suggestion: MeetingSuggestion,
  t: TFunction,
): MeetingPreviewFacts => ({
  id: suggestion.offer_id,
  title: t("meetings.detected.mayBeActive", {
    provider: t(meetingProviderKey(suggestion.provider)),
  }),
  origin: "app",
  startUtcMs: null,
  endUtcMs: null,
  calendarName: null,
  appName: t(meetingProviderKey(suggestion.provider)),
  attendeeCount: null,
  participants: [],
  description: null,
  url: null,
});
