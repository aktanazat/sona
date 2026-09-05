import * as React from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { HistoryTrendPoint, HistoryTrendProjection } from "@/bindings";
import { Microlabel, SettingsDisclosure } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import {
  ActivityBars,
  ActivitySparkline,
  ActivityWeek,
  type ActivityWeekDay,
} from "./ActivityBandCharts";
import { activityPage } from "./activityPaging";

export interface ActivityBandProps {
  trend: HistoryTrendProjection;
}

const parseLocalDate = (value: string): Date => new Date(`${value}T00:00:00`);

const isToday = (date: Date, today: Date): boolean =>
  date.getFullYear() === today.getFullYear() &&
  date.getMonth() === today.getMonth() &&
  date.getDate() === today.getDate();

const formatRange = (
  points: readonly HistoryTrendPoint[],
  formatter: Intl.DateTimeFormat,
): string => {
  const first = points[0];
  const last = points[points.length - 1];
  if (first === undefined || last === undefined) return "";
  return `${formatter.format(parseLocalDate(first.local_date))}–${formatter.format(parseLocalDate(last.local_date))}`;
};

/* The charts and the summary fact add the same two fields over a list of days;
 * only the span differs, so the addition itself lives in one place. */
const sumDays = (points: readonly HistoryTrendPoint[]) => {
  let recordings = 0;
  let words = 0;
  for (const point of points) {
    recordings += point.recordings;
    words += point.words;
  }
  return { recordings, words };
};

/* One chart inside the disclosure: what it counts, and the week drawn under it.
 * The totals used to sit here at 24px each; they are the summary's fact now, so
 * repeating them beside the shape they describe would say everything twice. */
const Measure: React.FC<{ label: string; children: React.ReactNode }> = ({
  label,
  children,
}) => (
  <div className="flex min-w-0 flex-col gap-2">
    <h3 className="min-w-0">
      <Microlabel>{label}</Microlabel>
    </h3>
    {children}
  </div>
);

/**
 * This week, as one closed row until somebody wants the shape of it.
 *
 * The fact is always this week's numbers, never the paged week's: the label
 * says "This week", and a summary that changed its numbers as you paged
 * backwards would be describing a week its own label denies. The range caption
 * inside says which week the charts are drawing.
 */
export function ActivityBand({ trend }: ActivityBandProps) {
  const { t, i18n } = useTranslation();
  const [pageIndex, setPageIndex] = React.useState(0);
  const selection = React.useMemo(
    () => activityPage(trend.points, pageIndex),
    [pageIndex, trend.points],
  );
  /* Page 0 whatever the charts are drawing: the fact belongs to the label, not
   * to the paged range. */
  const currentWeek = React.useMemo(
    () => activityPage(trend.points, 0).points,
    [trend.points],
  );
  const { page, start, points } = selection;
  const locale = i18n.resolvedLanguage ?? i18n.language;
  const dateFormat = React.useMemo(
    () =>
      new Intl.DateTimeFormat(locale, {
        month: "short",
        day: "numeric",
      }),
    [locale],
  );
  const weekdayFormat = React.useMemo(
    () => new Intl.DateTimeFormat(locale, { weekday: "long" }),
    [locale],
  );
  const narrowWeekdayFormat = React.useMemo(
    () => new Intl.DateTimeFormat(locale, { weekday: "narrow" }),
    [locale],
  );
  const weekdayListFormat = React.useMemo(
    () => new Intl.ListFormat(locale, { style: "long", type: "conjunction" }),
    [locale],
  );

  const dictations = points.map((point) => point.recordings);
  const words = points.map((point) => point.words);
  const week: ActivityWeekDay[] = [];
  const activeWeekdayNames: string[] = [];
  const today = new Date();
  const wordTotal = sumDays(points).words;
  let peakIndex = 0;
  for (let index = 0; index < points.length; index += 1) {
    const point = points[index];
    const localDate = parseLocalDate(point.local_date);
    const active = dictations[index] > 0;

    /* The trend projection has a streak total but no per-day streak payload.
     * Its Dictations bars already own the daily activity source, so the dot row
     * derives from that same recordings array rather than inventing a second
     * definition of an active day. */
    week.push({
      label: narrowWeekdayFormat.format(localDate),
      active,
      today: isToday(localDate, today),
    });
    if (active) activeWeekdayNames.push(weekdayFormat.format(localDate));

    if (point.recordings > (points[peakIndex]?.recordings ?? -1)) {
      peakIndex = index;
    }
  }

  const weekTotals = sumDays(currentWeek);

  const peak = points[peakIndex];
  const finalPoint = points[points.length - 1];
  const peakDay =
    peak === undefined
      ? ""
      : weekdayFormat.format(parseLocalDate(peak.local_date));
  const dateRange = formatRange(points, dateFormat);
  const activeWeekdays = weekdayListFormat.format(activeWeekdayNames) || "—";
  const previousLabel = t("overview.activity.rangePrevious", "Previous 7 days");
  const nextLabel = t("overview.activity.rangeNext", "Next 7 days");

  return (
    <SettingsDisclosure
      label={t("overview.week.title", "This week")}
      fact={[
        t("overview.week.dictations", { count: weekTotals.recordings }),
        t("overview.week.words", { count: weekTotals.words }),
        t("overview.week.streak", { count: trend.current_streak_days }),
      ].join(" · ")}
    >
      <div className="flex flex-col gap-4 px-6 py-5">
        {/* Which week the charts are drawing, and the two steps that change it. */}
        <div className="flex items-center gap-0.5">
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label={previousLabel}
            disabled={start === 0}
            onClick={() => setPageIndex((current) => current + 1)}
          >
            <ChevronLeft aria-hidden="true" />
          </Button>
          <span className="min-w-[11ch] text-center text-[13px] leading-[18px] text-gray-900 tabular-nums">
            {dateRange}
          </span>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label={nextLabel}
            disabled={page === 0}
            onClick={() => setPageIndex((current) => Math.max(0, current - 1))}
          >
            <ChevronRight aria-hidden="true" />
          </Button>
        </div>

        <div className="grid gap-6 sm:grid-cols-3">
          <Measure label={t("overview.activity.dictations", "Dictations")}>
            <ActivityBars
              values={dictations}
              weekdayLabels={week.map((day) => day.label)}
              ariaLabel={t(
                "overview.activity.dictationsAria",
                "Dictations per day, highest {{count}} on {{day}}",
                {
                  count: peak?.recordings ?? 0,
                  day: peakDay,
                },
              )}
            />
          </Measure>

          <Measure label={t("overview.activity.words", "Words")}>
            <ActivitySparkline
              values={words}
              ariaLabel={t(
                "overview.activity.wordsAria",
                "Words per day, {{count}} total, ending at {{last}}",
                {
                  count: wordTotal,
                  last: finalPoint?.words ?? 0,
                },
              )}
            />
          </Measure>

          <Measure label={t("overview.activity.streak", "Streak")}>
            <ActivityWeek
              days={week}
              ariaLabel={t(
                "overview.activity.streakAria",
                "Current streak, {{count}} days. Active days this week: {{days}}.",
                {
                  count: trend.current_streak_days,
                  days: activeWeekdays,
                },
              )}
            />
          </Measure>
        </div>
      </div>
    </SettingsDisclosure>
  );
}
