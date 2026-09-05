import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { ChevronDown } from "lucide-react";
import type {
  MeetingNotesTemplate,
  MeetingUpcomingRow,
  SourceKind,
} from "@/bindings";
import { Microlabel, SETTINGS_SURFACE } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Skeleton } from "@/components/vg/skeleton";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/vg/select";
import { Switch } from "@/components/vg/switch";
import { cn } from "@/lib/cn";
import {
  formatTimeOfDay,
  groupByLocalDay,
  localDayHeading,
} from "@/lib/utils/localDay";
import { MEETING_NOTES_TEMPLATES } from "../meetingAnalytics";
import {
  useUpcomingEvents,
  type UpcomingEventsState,
} from "./useUpcomingEvents";

/* D28: the week ahead, above the log of what already happened.
 *
 * Quiet rows, read by day, in the same grammar meeting history is written in —
 * the same day bucketer, the same headings, one surface for the whole list and
 * the same 62px gutter of clock times, so the two lists line up down the page.
 * What is different is that these rows are not history: they carry the three
 * decisions their series has made, and the only controls on the page that can
 * change them.
 *
 * Round 7 emptied the row down to what a person scanning the week asks for:
 * when, what it is called, and whose calendar it is on. The attendee names and
 * the "+3" count went with it — a row is not the place to read a guest list,
 * and the names were also the only reason this section mounted the People
 * dialog. "Repeats" stays as grey meta, because it is what explains the
 * chevron beside it.
 *
 * The section adds no scroll container. It is one more child of the page's
 * column, which is the pane's single scroll owner, because a fixed 900x800
 * window with two scrollbars in it is two places to lose your position.
 *
 * The "no calendar" state is deliberately not an error. Sona reads the calendar
 * macOS already holds — the Google, iCloud and Outlook accounts signed in
 * there — so the fix is a grant, said in one bronze line with the one place
 * that grants it as a link. */

/** The sentinel the picker uses for "no choice", since a Select has no empty. */
const APP_DEFAULT = "app-default";

const templateValue = (template: MeetingNotesTemplate | null): string =>
  template ?? APP_DEFAULT;

/* The Select hands its value back as a plain string. Rather than trusting that
 * string, it is looked up in the catalog the options were built from: the
 * sentinel is not in there, and neither is anything else, so "not a template"
 * and "no choice" are the same answer. */
const templateChoice = (value: string): MeetingNotesTemplate | null =>
  MEETING_NOTES_TEMPLATES.find((template) => template === value) ?? null;

export interface SeriesControlsProps {
  row: MeetingUpcomingRow;
  saving: boolean;
  onAlwaysRecord: (seriesKey: string, alwaysRecord: boolean) => void;
  onTemplate: (
    seriesKey: string,
    template: MeetingNotesTemplate | null,
  ) => void;
  onDigest: (seriesKey: string, included: boolean) => void;
}

/* The three decisions, on the series rather than on this occurrence. They are
 * behind a disclosure because a calendar row's job is to say what is next, and
 * three switches per row would make the section a settings page with dates on
 * it. */
export const SeriesControls: React.FC<SeriesControlsProps> = ({
  row,
  saving,
  onAlwaysRecord,
  onTemplate,
  onDigest,
}) => {
  const { t } = useTranslation();
  const series = row.series;
  if (series === null) return null;

  return (
    <div
      data-slot="upcoming-series-controls"
      className="flex flex-col gap-3 border-t border-gray-alpha-400 px-6 py-3.5"
    >
      <div className="flex items-center justify-between gap-6">
        <span className="text-[14px] leading-[21px] text-gray-1000">
          {t("meetings.upcoming.alwaysRecord")}
        </span>
        <Switch
          aria-label={t("meetings.upcoming.alwaysRecord")}
          checked={series.always_record}
          disabled={saving}
          onCheckedChange={(next) => onAlwaysRecord(series.series_key, next)}
        />
      </div>

      <div className="flex items-center justify-between gap-6">
        <span className="text-[14px] leading-[21px] text-gray-1000">
          {t("meetings.upcoming.template")}
        </span>
        <Select
          value={templateValue(series.template)}
          disabled={saving}
          onValueChange={(value) =>
            onTemplate(series.series_key, templateChoice(value))
          }
        >
          <SelectTrigger
            size="sm"
            className="w-auto"
            aria-label={t("meetings.upcoming.template")}
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={APP_DEFAULT}>
              {t("meetings.upcoming.templateDefault")}
            </SelectItem>
            {MEETING_NOTES_TEMPLATES.map((template) => (
              <SelectItem key={template} value={template}>
                {t(`meetings.notes.templates.${template}`)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <div className="flex items-center justify-between gap-6">
        <span className="text-[14px] leading-[21px] text-gray-1000">
          {t("meetings.upcoming.digest")}
        </span>
        <Switch
          aria-label={t("meetings.upcoming.digest")}
          checked={series.digest_included}
          disabled={saving}
          onCheckedChange={(next) => onDigest(series.series_key, next)}
        />
      </div>
    </div>
  );
};

interface UpcomingRowProps extends SeriesControlsProps {
  expanded: boolean;
  onToggleExpanded: () => void;
}

const UpcomingRow: React.FC<UpcomingRowProps> = ({
  row,
  expanded,
  onToggleExpanded,
  ...controls
}) => {
  const { t } = useTranslation();

  return (
    <li data-slot="upcoming-row" className="group flex flex-col">
      <div className="flex items-baseline gap-5 px-6 py-3.5">
        {/* When, in its own gutter. Tabular so a column of clock times keeps
         * one right edge instead of jittering with the digits, and 62px wide
         * so the recorded list below lines up with it. */}
        <Microlabel className="w-[62px] flex-none text-end tabular-nums">
          {formatTimeOfDay(row.start_utc_ms)}
        </Microlabel>

        <span className="flex min-w-0 flex-1 items-baseline gap-2">
          <span className="truncate text-[14px] leading-[21px] font-medium text-gray-1000">
            {row.title}
          </span>
          {row.calendar_name === null ? null : (
            <span
              data-slot="upcoming-calendar"
              className="truncate text-[13px] leading-[18px] text-gray-800"
            >
              {row.calendar_name}
            </span>
          )}
          {row.series === null ? null : (
            <span
              data-slot="upcoming-series-chip"
              className="flex-none text-[13px] leading-[18px] text-gray-800"
            >
              {t("meetings.upcoming.recurring")}
            </span>
          )}
        </span>

        {/* The disclosure, on the rows that have something to disclose. It
         * appears on hover, on keyboard focus anywhere in the row and while
         * the row is open, so a week of calendar rows is not also a column of
         * chevrons. */}
        {row.series === null ? null : (
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            className={cn(
              "-me-1.5 flex-none self-center text-gray-800 opacity-0 transition-opacity",
              "group-hover:opacity-100 group-focus-within:opacity-100",
              "focus-visible:opacity-100 aria-expanded:opacity-100",
              "motion-reduce:transition-none",
            )}
            aria-expanded={expanded}
            aria-label={t("meetings.upcoming.seriesOptions", {
              title: row.title,
            })}
            onClick={onToggleExpanded}
          >
            <ChevronDown
              aria-hidden="true"
              className={cn(
                "size-4 transition-transform motion-reduce:transition-none",
                expanded && "rotate-180",
              )}
            />
          </Button>
        )}
      </div>
      {expanded ? <SeriesControls row={row} {...controls} /> : null}
    </li>
  );
};

const UpcomingSkeleton: React.FC<{ label: string }> = ({ label }) => (
  <div role="status" aria-label={label} className={SETTINGS_SURFACE}>
    {[0, 1].map((row) => (
      <div key={row} className="flex items-center gap-5 px-6 py-3.5">
        <Skeleton className="h-3.5 w-12" />
        <Skeleton className="h-3.5 flex-1" />
      </div>
    ))}
  </div>
);

export interface MeetingsUpcomingViewProps
  extends Pick<
    UpcomingEventsState,
    | "events"
    | "loading"
    | "saving"
    | "setAlwaysRecord"
    | "setTemplate"
    | "setDigestIncluded"
  > {
  /** The shell's route setter, so a missing grant can name where it is given. */
  onOpenSettings?: () => void;
}

/** The section, rendered from state alone, so every one of its states is one
 *  prop away in a test. */
export const MeetingsUpcomingView: React.FC<MeetingsUpcomingViewProps> = ({
  events,
  loading,
  saving,
  onOpenSettings,
  setAlwaysRecord,
  setTemplate,
  setDigestIncluded,
}) => {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState<string | null>(null);

  const title = t("meetings.upcoming.title");
  const rows = events?.rows ?? [];
  const days = groupByLocalDay(rows, (row) => row.start_utc_ms);
  /* A read that failed and a calendar that cannot be read are the same thing
   * to a reader: the section cannot say what is next. It says so once. */
  const access = events?.access ?? "denied";

  return (
    <section data-slot="meetings-upcoming" className="flex flex-col gap-2">
      <h2 className="min-h-8 content-center">
        <Microlabel>{title}</Microlabel>
      </h2>

      {loading ? (
        <UpcomingSkeleton label={t("meetings.upcoming.loading")} />
      ) : rows.length === 0 ? (
        access === "authorized" || access === "unavailable" ? (
          <p className="text-[13px] leading-5 text-gray-800">
            {access === "authorized"
              ? t("meetings.upcoming.empty")
              : t("meetings.upcoming.unavailable")}
          </p>
        ) : (
          /* One bronze line. It names the fix in words either way - a region
           * that cannot say what is next still owes the reader the next
           * action - and wraps them in a press only where this mount was
           * given a route to Settings. */
          <p className="text-[13px] leading-5 text-accent-strong">
            {t("meetings.upcoming.noAccess")}{" "}
            {onOpenSettings === undefined ? (
              t("meetings.upcoming.noAccessFix")
            ) : (
              <button
                type="button"
                onClick={onOpenSettings}
                className="rounded-md underline underline-offset-2 hover:text-gray-1000"
              >
                {t("meetings.upcoming.noAccessFix")}
              </button>
            )}
          </p>
        )
      ) : (
        <div className={SETTINGS_SURFACE}>
          {days.map((day) => {
            const heading = localDayHeading(day.startOfDayMs, t);
            return (
              <React.Fragment key={day.startOfDayMs}>
                <h3 data-slot="upcoming-day" className="px-6 pt-2.5 pb-2">
                  <Microlabel>{heading}</Microlabel>
                </h3>
                <ul
                  role="list"
                  aria-label={heading}
                  className="divide-y divide-gray-alpha-400"
                >
                  {day.items.map((row) => (
                    <UpcomingRow
                      key={row.event_key}
                      row={row}
                      saving={saving === row.series?.series_key}
                      expanded={expanded === row.event_key}
                      onToggleExpanded={() =>
                        setExpanded((current) =>
                          current === row.event_key ? null : row.event_key,
                        )
                      }
                      onAlwaysRecord={(seriesKey, next) =>
                        void setAlwaysRecord(seriesKey, next)
                      }
                      onTemplate={(seriesKey, template) =>
                        void setTemplate(seriesKey, template)
                      }
                      onDigest={(seriesKey, next) =>
                        void setDigestIncluded(seriesKey, next)
                      }
                    />
                  ))}
                </ul>
              </React.Fragment>
            );
          })}
        </div>
      )}
    </section>
  );
};

/** The connected section Meetings home mounts. `sources` is not rendered here:
 *  it is the acknowledgement a standing always-record grant has to name. */
export const MeetingsUpcoming: React.FC<{
  sources: SourceKind[];
  onOpenSettings?: () => void;
}> = ({ sources, onOpenSettings }) => {
  const state = useUpcomingEvents(sources);
  return <MeetingsUpcomingView onOpenSettings={onOpenSettings} {...state} />;
};
