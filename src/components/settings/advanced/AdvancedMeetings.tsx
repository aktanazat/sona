import React, { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { SettingsLinkRow, SettingsSection } from "@/components/settings/rows";
import {
  MeetingDetectionAdvanced,
  MeetingDetectionState,
} from "../meetings/MeetingDetectionSettings";
import { MeetingAutomations } from "../meetings/MeetingAutomations";
import { MeetingDigestSettings } from "../meetings/MeetingDigestSettings";
import { MeetingRetentionSettings } from "../meetings/MeetingRetention";
import { MeetingRemoteIntelligence } from "../meetings/MeetingRemoteIntelligence";
import { MeetingTrackersSettings } from "../meetings/MeetingTrackersSettings";

export const AdvancedMeetings: React.FC<{
  onOpenPrompts: () => void;
  revealRequest?: number;
}> = ({ onOpenPrompts, revealRequest }) => {
  const { t } = useTranslation();
  const sectionRef = useRef<HTMLElement>(null);

  useEffect(() => {
    if (revealRequest === undefined) return;
    sectionRef.current?.scrollIntoView({ block: "start" });
  }, [revealRequest]);

  return (
    <>
      <SettingsSection
        ref={sectionRef}
        label={t("settingsV2.advanced.meetings")}
      >
        <MeetingDetectionAdvanced />
        <MeetingRetentionSettings />
        <MeetingDigestSettings />
        <MeetingTrackersSettings />
        <SettingsLinkRow
          label={t("prompts.title")}
          action={t("common.open")}
          onOpen={onOpenPrompts}
        />
      </SettingsSection>
      <MeetingDetectionState />
      <MeetingRemoteIntelligence />
      <MeetingAutomations />
    </>
  );
};
