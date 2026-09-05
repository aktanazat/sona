import React from "react";
import { useTranslation } from "react-i18next";
import {
  Microlabel,
  SettingsDisclosure,
  SettingsSurface,
} from "@/components/settings/rows";
import {
  formatPatience,
  formatTalkDuration,
  formatTalkShare,
  type MeetingAnalytics,
} from "./meetingAnalytics";
import { talkTimeSlices } from "./review/talkTime";

/* Three numbers about how the conversation went, derived from the diarized
 * transcript and nothing else, plus the per-speaker split and whatever the
 * user's keyword trackers found. Every figure is a fact about the transcript
 * on this screen, so a meeting with no speech shows nothing at all rather
 * than a row of zeros.
 *
 * Closed by default, because this is not what a person opened the meeting to
 * read. The summary carries who did the talking, which is the one measurement
 * out of all of these that anybody checks twice, and it is the fact that
 * decides whether the rest is worth opening.
 *
 * There is no "Top talker" tile: the split inside is sorted by share, so its
 * first row already prints the leader's percentage and their name. The tile
 * was the same number twice on one screen.
 *
 * Tiles separated by hairlines, not three cards: these numbers are read
 * together and boxing each one would claim they are three separate objects. */

interface MeetingAnalyticsStripProps {
  analytics: MeetingAnalytics;
  speakerNames: Record<string, string>;
  onJumpToSegment: (segmentId: string) => void;
}

interface StatProps {
  label: string;
  value: string;
  /** A second fact, never the first one again: who, or a different count. */
  detail?: string;
}

const Stat: React.FC<StatProps> = ({ label, value, detail }) => (
  <div className="flex flex-col gap-0.5 px-6 py-4">
    <Microlabel>{label}</Microlabel>
    {/* A measurement, not a headline: the page's own title is the largest
     * type on it, and three 18px figures in a strip were competing with it. */}
    <p className="text-[16px] leading-[25px] font-medium tabular-nums text-gray-1000">
      {value}
    </p>
    {detail ? (
      <p className="truncate text-[13px] leading-[18px] text-gray-800">
        {detail}
      </p>
    ) : null}
  </div>
);

export const MeetingAnalyticsStrip: React.FC<MeetingAnalyticsStripProps> = ({
  analytics,
  speakerNames,
  onJumpToSegment,
}) => {
  const { t } = useTranslation();
  const { talk, trackers } = analytics;
  const nameOf = (speakerId: string | null) =>
    speakerId === null
      ? t("meetings.analytics.unknownSpeaker", "Unknown speaker")
      : (speakerNames[speakerId] ??
        t("meetings.analytics.unknownSpeaker", "Unknown speaker"));

  /* Nothing was said, so there is nothing to measure and nothing to open. A
   * disclosure over an empty body is a row that asks to be pressed twice. */
  if (talk.segment_count === 0) return null;

  /* Who held the floor, widest share first — the same order the split inside
   * is drawn in, so opening this expands the summary rather than answering a
   * different question. Two names at most: a summary is a glance. */
  const leaders = talkTimeSlices(talk.speakers, nameOf)
    .slice(0, 2)
    .map((slice) => `${slice.name} ${formatTalkShare(slice.permille)}`)
    .join(" · ");

  return (
    <SettingsSurface className="overflow-hidden">
      <SettingsDisclosure
        label={t("meetings.analytics.title", "Conversation")}
        fact={leaders.length === 0 ? undefined : leaders}
      >
        <div className="grid grid-cols-3 divide-x divide-gray-alpha-400">
          <Stat
            label={t(
              "meetings.analytics.longestMonologue",
              "Longest monologue",
            )}
            value={formatTalkDuration(talk.longest_monologue_ns)}
            detail={nameOf(talk.longest_monologue_speaker_id)}
          />
          <Stat
            label={t("meetings.analytics.patience", "Patience")}
            value={formatPatience(talk.median_switch_gap_ms)}
            /* Not a restatement of "Patience": it is what the number measures,
             * which the word alone does not say. */
            detail={t(
              "meetings.analytics.patienceDetail",
              "Median pause before replying",
            )}
          />
          <Stat
            label={t("meetings.analytics.interactions", "Handovers")}
            value={String(talk.interaction_count)}
            /* A different count, not the same one twice: turns are every stretch
             * of speech, handovers are the ones that changed speaker. */
            detail={t("meetings.analytics.turns", "{{count}} turns", {
              count: talk.turn_count,
            })}
          />
        </div>

        <ul className="divide-y divide-gray-alpha-400">
          {talk.speakers.map((share) => (
            <li
              key={share.speaker_id}
              className="flex items-center justify-between gap-4 px-6 py-3"
            >
              <span className="min-w-0 truncate text-[14px] leading-[21px] text-gray-1000">
                {nameOf(share.speaker_id)}
              </span>
              <span className="flex flex-none items-baseline gap-3">
                <span className="text-[14px] leading-[21px] font-medium tabular-nums text-gray-1000">
                  {formatTalkShare(share.share_permille)}
                </span>
                <Microlabel className="tabular-nums text-gray-800">
                  {t(
                    "meetings.analytics.speakerDetail",
                    "{{time}} · {{turns}}",
                    {
                      time: formatTalkDuration(share.speaking_ns),
                      turns: t("meetings.analytics.turns", "{{count}} turns", {
                        count: share.turn_count,
                      }),
                    },
                  )}
                </Microlabel>
              </span>
            </li>
          ))}
        </ul>

        {trackers.length === 0 ? null : (
          <div className="flex flex-col gap-2 py-3">
            <h3 className="px-6">
              <Microlabel>
                {t("meetings.analytics.trackers", "Trackers")}
              </Microlabel>
            </h3>
            <ul className="divide-y divide-gray-alpha-400 border-t border-gray-alpha-400">
              {trackers.map((tracker) => (
                <li
                  key={tracker.name}
                  className="flex items-center justify-between gap-4 px-6 py-3"
                >
                  <span className="min-w-0 truncate text-[14px] leading-[21px] text-gray-1000">
                    {tracker.name}
                  </span>
                  {tracker.hit_count === 0 ? (
                    <Microlabel className="text-gray-800">
                      {t("meetings.analytics.noHits", "Not mentioned")}
                    </Microlabel>
                  ) : (
                    <span className="flex flex-none items-baseline gap-3">
                      <Microlabel className="tabular-nums text-gray-800">
                        {t("meetings.analytics.hits", "{{count}} mentions", {
                          count: tracker.hit_count,
                        })}
                      </Microlabel>
                      <button
                        type="button"
                        onClick={() => onJumpToSegment(tracker.segment_ids[0])}
                        className="cursor-pointer rounded-md text-[13px] leading-[18px] text-accent-strong underline-offset-2 transition-colors hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none motion-reduce:transition-none"
                      >
                        {t("meetings.analytics.jumpToFirst", "Show first")}
                      </button>
                    </span>
                  )}
                </li>
              ))}
            </ul>
          </div>
        )}
      </SettingsDisclosure>
    </SettingsSurface>
  );
};
