import React from "react";
import { useTranslation } from "react-i18next";
import { SettingsSection } from "@/components/settings/rows";
import {
  MeetingDetectionAdvanced,
  MeetingDetectionState,
} from "../meetings/MeetingDetectionSettings";
import { MeetingAutomations } from "../meetings/MeetingAutomations";
import { MeetingDigestSettings } from "../meetings/MeetingDigestSettings";
import { MeetingPrompts } from "../meetings/MeetingPrompts";
import { MeetingRetentionSettings } from "../meetings/MeetingRetention";
import { MeetingRemoteIntelligence } from "../meetings/MeetingRemoteIntelligence";
import { MeetingTrackersSettings } from "../meetings/MeetingTrackersSettings";

/* Everything about meetings that is not the switch on Essentials.
 *
 * The Meetings page used to carry this as a settings tail under its own
 * history, which is why detection had two homes. It has one now: the master
 * switch and the app list are Essentials, and what widens the evidence, how
 * long a meeting is kept, what the operator's own phrase lists are, and what
 * detection can currently see are all here.
 *
 * The watch list lost its heading in round 7: one closed row inside this
 * section, with the phrases on the summary, instead of a section of its own
 * holding two text fields per phrase. */
export const AdvancedMeetings: React.FC = () => {
  const { t } = useTranslation();

  return (
    <>
      <SettingsSection label={t("settingsV2.advanced.meetings")}>
        <MeetingDetectionAdvanced />
        <MeetingRetentionSettings />
        <MeetingDigestSettings />
        <MeetingTrackersSettings />
      </SettingsSection>
      <MeetingDetectionState />
      <MeetingRemoteIntelligence />
      <MeetingAutomations />
      <MeetingPrompts />
    </>
  );
};
