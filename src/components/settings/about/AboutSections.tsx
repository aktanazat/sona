import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ExternalLink } from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { commands } from "@/bindings";
import {
  SettingsDisclosure,
  SettingsRow,
  SettingsSection,
} from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { AppDataDirectory } from "../AppDataDirectory";
import { AppLanguageSelector } from "../AppLanguageSelector";
import { MaterialSelector } from "../MaterialSelector";
import { ThemeSelector } from "../ThemeSelector";
import { LogDirectory } from "../debug/LogDirectory";
import { UpdateRows, type VersionState } from "./UpdateRows";

const REPOSITORY_URL = "https://github.com/aktanazat/sona";

const openLicenseNotices = async () => {
  const result = await commands.openLicenseNotices();
  if (result.status === "error") {
    console.error("Failed to open bundled notices:", result.error);
  }
};

const openExternal = async (url: string) => {
  try {
    await openUrl(url);
  } catch (error) {
    console.error("Failed to open link:", error);
  }
};

/* What build this is, where it came from, and where it keeps things.
 *
 * The last section of Advanced, and the only place in the app allowed to print
 * a version or a build fact. It was a tab of its own with four sections; the
 * acknowledgment paragraph and the upstream-repository row are gone, because
 * the bundled license notices the source row opens are the authoritative
 * version of both.
 *
 * It leads with three rows that are not build facts: the language Sona
 * speaks, its appearance, and the material its windows are made of. Each is
 * set once and then never again, and this is the least prominent section on
 * the page — which is the whole argument for putting them here rather than on
 * Essentials, where a once-in-an-install choice would sit beside the
 * microphone. */
export const AboutSections: React.FC = () => {
  const { t } = useTranslation();
  const [version, setVersion] = useState<VersionState>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;

    const fetchVersion = async () => {
      try {
        const appVersion = await getVersion();
        if (!cancelled) setVersion({ kind: "ready", version: appVersion });
      } catch (error) {
        console.error("Failed to get app version:", error);
        if (!cancelled) setVersion({ kind: "unavailable" });
      }
    };

    void fetchVersion();
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <SettingsSection label={t("settingsV2.advanced.about")}>
      <AppLanguageSelector />
      <ThemeSelector />
      <MaterialSelector />
      <UpdateRows version={version} />
      <SettingsRow
        label={t("settings.about.source.repository")}
        hint={t("settings.about.source.license")}
      >
        {/* The scheme is the one part of this URL that tells the reader
         * nothing, so only the identifying part is shown. The button still
         * opens the whole URL, and the accessible name still carries it.
         *
         * A URL is a value, not a label: one step under the 14px value tier,
         * on the same 13px step as a `Microlabel`. A rem-based `text-xs` would
         * land it at 10.5px against this app's 14px root, below the labels it
         * sits beside. */}
        <span className="max-w-[260px] truncate text-[13px] text-gray-900">
          {REPOSITORY_URL.replace("https://", "")}
        </span>
        <Button
          variant="outline"
          size="sm"
          aria-label={`${t("common.open")} ${REPOSITORY_URL}`}
          onClick={() => void openExternal(REPOSITORY_URL)}
        >
          <ExternalLink aria-hidden="true" />
          {t("common.open")}
        </Button>
      </SettingsRow>
      <SettingsRow label={t("settings.about.sourceCode.title")}>
        <Button
          variant="outline"
          size="sm"
          onClick={() => void openLicenseNotices()}
        >
          {t("settings.about.sourceCode.button")}
        </Button>
      </SettingsRow>
      {/* Two absolute paths, each long enough to wrap. Nobody reads them on
       * the way past; they are opened when something has gone wrong. */}
      <SettingsDisclosure label={t("settings.about.files.title")}>
        <AppDataDirectory />
        <LogDirectory />
      </SettingsDisclosure>
    </SettingsSection>
  );
};
