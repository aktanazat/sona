import React from "react";
import { useTranslation } from "react-i18next";
import { formatModelSize } from "@/lib/utils/format";
import {
  SettingsDisclosure,
  SettingsLinkRow,
  SettingsSection,
} from "@/components/settings/rows";
import { useSettings } from "@/hooks/useSettings";
import { useModelStore } from "@/stores/modelStore";
import { CLOUD_STT_PROVIDERS } from "@/lib/cloudStt";
import { PostProcessingSettingsApi } from "../PostProcessingSettingsApi";
import { CloudSttProviderSettings } from "../models/CloudSttProviderSettings";
import { diskUsage } from "../models/modelCatalog";

/* Where models are chosen and what they may talk to.
 *
 * Model choice is automatic now — the sidebar chip that used to state it is
 * gone — so this section is a door to the catalog rather than a copy of it,
 * with the disk cost as the fact that decides whether you open it.
 *
 * The two credential blocks below it are one-time setups, so they are rows
 * until a reader needs them: a cloud transcription key and a remote cleanup
 * endpoint are things you configure once, and laid out flat they would bury
 * every setting around them. Each summary states what is set up - which keys
 * exist, which endpoint is selected - so opening one is a decision, not a
 * check. Both facts read the settings store the bodies write, so there is one
 * source for the claim and the form behind it. */
export const AdvancedModels: React.FC<{ onOpenCatalog: () => void }> = ({
  onOpenCatalog,
}) => {
  const { t } = useTranslation();
  const { settings } = useSettings();
  const models = useModelStore((state) => state.models);
  const onDisk = diskUsage(models);
  const savedCloudKeys = CLOUD_STT_PROVIDERS.filter((candidate) =>
    settings?.cloud_stt_providers?.some(
      (entry) =>
        entry.provider === candidate.provider && entry.secret_state?.configured,
    ),
  ).map((candidate) => t(candidate.labelKey));
  const cleanupProvider = settings?.post_process_providers?.find(
    (provider) => provider.id === settings?.post_process_provider_id,
  );

  return (
    <SettingsSection label={t("settingsV2.advanced.models")}>
      <SettingsLinkRow
        label={t("settingsV2.advanced.modelCatalog")}
        action={t("common.open")}
        fact={
          onDisk.count > 0
            ? `${t("settings.models.familyCount", { total: onDisk.count })} \u00b7 ${formatModelSize(onDisk.sizeMb)}`
            : undefined
        }
        onOpen={onOpenCatalog}
      />
      <SettingsDisclosure
        label={t("settingsV2.advanced.cloudKeys")}
        fact={
          savedCloudKeys.length === 0
            ? t("common.none")
            : savedCloudKeys.join(", ")
        }
      >
        <CloudSttProviderSettings />
      </SettingsDisclosure>
      <SettingsDisclosure
        label={t("settingsV2.advanced.cleanupProvider")}
        fact={cleanupProvider?.label ?? t("common.none")}
        lazy
      >
        <PostProcessingSettingsApi />
      </SettingsDisclosure>
    </SettingsSection>
  );
};
