import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import type { SourceKind } from "@/bindings";
import type { MeetingStartOptions } from "../meetingTypes";

/* What every press of Start records. Round 7 took the two capture chips off
 * Meetings home, and with them the only control that ever changed this: a
 * page-local answer to "which sources" that reset on every mount was a
 * setting pretending to be a decision, and one press with both sources armed
 * is the answer the page had anyway. A source that is unavailable is the start
 * gate's subject, not this list's - it names the one that failed and offers
 * the two ways out. */
const MEETING_SOURCES: SourceKind[] = ["microphone", "system_audio"];

/** Fills in everything a press of Start records that the press itself does not
 *  say: the sources it will ask for, and the defaults that never change. */
export type MeetingStartOptionsBuilder = (
  origin: MeetingStartOptions["origin"],
  suggestionId?: MeetingStartOptions["suggestionId"],
  title?: string,
  preview?: MeetingStartOptions["preview"],
  calendarEventKey?: MeetingStartOptions["calendarEventKey"],
) => MeetingStartOptions;

export interface MeetingStartSetup {
  sources: SourceKind[];
  startOptions: MeetingStartOptionsBuilder;
}

export const useMeetingStartSetup = (): MeetingStartSetup => {
  const { t } = useTranslation();

  const startOptions = useCallback(
    (
      origin: MeetingStartOptions["origin"],
      suggestionId: MeetingStartOptions["suggestionId"] = null,
      title = t("meetings.setup.defaultTitle"),
      preview: MeetingStartOptions["preview"] = null,
      calendarEventKey: MeetingStartOptions["calendarEventKey"] = null,
    ): MeetingStartOptions => ({
      title,
      origin,
      suggestionId,
      sources: MEETING_SOURCES,
      calendarEventKey,
      degradedStartPolicy: "abort_if_required_source_fails",
      destination: { kind: "local" },
      preview,
    }),
    [t],
  );

  return { sources: MEETING_SOURCES, startOptions };
};
