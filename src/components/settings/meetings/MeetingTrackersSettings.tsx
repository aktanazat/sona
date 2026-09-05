import React, { useEffect, useState } from "react";
import { toast } from "sonner";
import { Plus, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import { RowActions, SettingsDisclosure } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Input } from "@/components/vg/input";
import {
  listKeywordTrackers,
  saveKeywordTrackers,
  type KeywordTracker,
} from "./meetingAnalytics";
import { summarizeNames } from "./meetingUtils";

/* Watch lists for words that matter to you. Every finished meeting transcript
 * is scanned for them on this Mac, and the hits show up on the meeting's own
 * Insights tab.
 *
 * A closed row on Advanced, not a section: a list of phrases somebody typed
 * once is the definition of a setting nobody reads again, and open it was a
 * heading plus two text fields per phrase. The names ride on the summary, so
 * the answer to "what am I watching for" needs no click.
 *
 * Patterns are literal phrases, not patterns in the regular-expression sense:
 * "is that your best price?" is a phrase somebody says, and typing it should
 * never produce a syntax error. The placeholder is where that is said, because
 * three lowercase phrases demonstrate it in less space than a sentence about
 * it did. */

/** Patterns are edited as one comma-separated line, which is how people list
 *  phrases. Commas inside a phrase are not supported, and do not need to be. */
const PATTERN_SEPARATOR = ", ";

export const MeetingTrackersSettings: React.FC = () => {
  const { t } = useTranslation();
  const [trackers, setTrackers] = useState<KeywordTracker[] | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    let active = true;
    listKeywordTrackers()
      .then((loaded) => {
        if (active) setTrackers(loaded);
      })
      .catch(() => {
        if (active) setTrackers([]);
      });
    return () => {
      active = false;
    };
  }, []);

  const commit = async (next: KeywordTracker[]) => {
    setTrackers(next);
    setSaving(true);
    try {
      setTrackers(await saveKeywordTrackers(next));
    } catch {
      toast.error(
        t(
          "meetings.analytics.trackersSaveFailed",
          "Sona could not save the trackers. Try again.",
        ),
      );
    } finally {
      setSaving(false);
    }
  };

  const edit = (index: number, tracker: KeywordTracker) => {
    if (trackers === null) return;
    setTrackers(trackers.map((item, at) => (at === index ? tracker : item)));
  };

  if (trackers === null) {
    return null;
  }

  /* What the summary says: the names, which are what a person recognises, and
   * a count that covers every row. A tracker being edited has no name yet, so
   * a roster of only-blank rows falls back to its size rather than claiming
   * there is nothing here, and a half-named roster still counts the blanks in
   * its "+N" instead of hiding them. */
  const named = trackers
    .map((tracker) => tracker.name.trim())
    .filter((name) => name !== "");
  const fact =
    trackers.length === 0
      ? t("common.none")
      : named.length === 0
        ? /* Mid-typing: rows exist, none of them have a name yet. The size is
           * the only true thing to say, and it is a number in the same column
           * the "+N" below lands in. */
          String(trackers.length)
        : summarizeNames(named, trackers.length);

  return (
    <SettingsDisclosure
      label={t("meetings.analytics.trackersTitle")}
      fact={fact}
    >
      {trackers.length === 0 ? (
        /* What fills the list and how, in one line. It replaces a centred
         * radar glyph over the same sentence: the icon said nothing the
         * sentence did not. */
        <p className="px-6 py-3.5 text-[13px] leading-5 text-gray-800">
          {t("meetings.analytics.noTrackers")}
        </p>
      ) : (
        <ul className="divide-y divide-gray-alpha-400">
          {trackers.map((tracker, index) => (
            <li
              key={index}
              className="group/row flex flex-wrap items-center gap-2 px-6 py-2.5"
            >
              <Input
                value={tracker.name}
                onChange={(event) =>
                  edit(index, { ...tracker, name: event.target.value })
                }
                onBlur={() => void commit(trackers)}
                placeholder={t("meetings.analytics.trackerName", "Name")}
                aria-label={t("meetings.analytics.trackerName", "Name")}
                disabled={saving}
                className="h-8 w-40 flex-none text-[14px]"
              />
              <Input
                value={tracker.patterns.join(PATTERN_SEPARATOR)}
                onChange={(event) =>
                  edit(index, {
                    ...tracker,
                    patterns: event.target.value.split(","),
                  })
                }
                onBlur={() => void commit(trackers)}
                placeholder={t(
                  "meetings.analytics.trackerPatterns",
                  "discount, best price, too expensive",
                )}
                aria-label={t(
                  "meetings.analytics.trackerPatternsLabel",
                  "Phrases, separated by commas",
                )}
                disabled={saving}
                className="h-8 min-w-48 flex-1 text-[14px]"
              />
              <RowActions className="flex-none">
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  className="text-gray-700 hover:text-red-900"
                  aria-label={t(
                    "meetings.analytics.removeTracker",
                    "Remove tracker",
                  )}
                  onClick={() =>
                    void commit(trackers.filter((_, at) => at !== index))
                  }
                  disabled={saving}
                >
                  <Trash2 aria-hidden="true" />
                </Button>
              </RowActions>
            </li>
          ))}
        </ul>
      )}
      <div className="flex justify-end px-6 py-3">
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => setTrackers([...trackers, { name: "", patterns: [] }])}
          disabled={saving}
        >
          <Plus aria-hidden="true" />
          {t("meetings.analytics.addTracker")}
        </Button>
      </div>
    </SettingsDisclosure>
  );
};
