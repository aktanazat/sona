import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "@/hooks/useSettings";
import { cn } from "@/lib/cn";
import { PAGE_COLUMN } from "./rows";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/vg/tabs";
import { EssentialsSettings } from "./essentials/EssentialsSettings";
import { AdvancedSettings } from "./advanced/AdvancedSettings";
import { DebugSettings } from "./debug/DebugSettings";
import { PromptLibrary } from "./prompts/PromptLibrary";
import type {
  SettingsNavigationRequest,
  SettingsNavigationTarget,
} from "./navigation";

type SettingsTab = SettingsNavigationTarget["tab"];

const BASE_TABS: readonly SettingsTab[] = ["essentials", "advanced", "prompts"];

const TAB_LABELS = {
  essentials: "settingsV2.tabs.essentials",
  advanced: "settingsV2.tabs.advanced",
  prompts: "prompts.title",
  debug: "settingsV2.tabs.debug",
} as const satisfies Record<SettingsTab, string>;

/* Debug is an extra tab only on a build the chord has unlocked, which is why
 * it is appended rather than declared: it is instrumentation, and a person who
 * has not asked for it should not be able to see that it exists.
 *
 * The strip is a hairline the width of the window with an underline mark under
 * the active tab. Its labels sit on the same 760px column as the page below, so
 * the first tab and the page title share a left edge. The mark is the kit's own
 * line variant, not a hand-rolled bottom border: that variant parks its bar
 * one pixel under the list, which is exactly where this container's hairline
 * is, so the mark reads as a break in the rule. Pulled to a single pixel in
 * the accent, because a 2px bar in the text colour is a second heading; the
 * active label carries the weight instead. */
const TAB_TRIGGER =
  "flex-none px-0 text-[14px] leading-[21px] font-normal text-gray-900 transition-colors hover:text-gray-1000 focus-visible:border-transparent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none motion-reduce:transition-none data-[state=active]:font-medium data-[state=active]:text-gray-1000 after:bg-primary group-data-[orientation=horizontal]/tabs:after:h-px";

export const SettingsHub: React.FC<{
  navigationRequest?: SettingsNavigationRequest | null;
  /**
   * The shell's section setter for settings links to hand off to. Optional for
   * the same reason every other section's shell callback is: this page renders
   * standalone in tests, where there is no shell to route with, and the
   * registry in sidebarSections types every destination as a bare component.
   */
  onOpenSection?: (
    section: "modes" | "models" | "settings",
    target?: SettingsNavigationTarget,
  ) => void;
}> = ({ navigationRequest, onOpenSection }) => {
  const { t } = useTranslation();
  const { settings } = useSettings();
  const [activeTab, setActiveTab] = useState<SettingsTab>(
    navigationRequest?.target.tab ?? "essentials",
  );
  const tabs = useMemo(
    () => (settings?.debug_mode ? [...BASE_TABS, "debug" as const] : BASE_TABS),
    [settings?.debug_mode],
  );
  const visibleTab = tabs.includes(activeTab) ? activeTab : "essentials";

  useEffect(() => {
    const requestedTab = navigationRequest?.target.tab ?? "essentials";
    setActiveTab(tabs.includes(requestedTab) ? requestedTab : "essentials");
  }, [navigationRequest?.nonce, tabs]);

  const advancedRequest =
    navigationRequest?.target.tab === "advanced" ? navigationRequest : null;

  return (
    <Tabs
      data-testid="settings-hub"
      value={visibleTab}
      onValueChange={(id) => {
        const next = tabs.find((tab) => tab === id);
        if (next) setActiveTab(next);
      }}
      className="gap-0"
    >
      <div className="border-b border-gray-alpha-400">
        <TabsList
          variant="line"
          aria-label={t("settingsV2.navigation")}
          className={cn(PAGE_COLUMN, "justify-start gap-6")}
        >
          {tabs.map((tab) => (
            <TabsTrigger key={tab} value={tab} className={TAB_TRIGGER}>
              {t(TAB_LABELS[tab])}
            </TabsTrigger>
          ))}
        </TabsList>
      </div>
      <TabsContent value="essentials">
        <EssentialsSettings />
      </TabsContent>
      <TabsContent value="advanced">
        <AdvancedSettings
          navigationRequest={advancedRequest}
          onOpenCatalog={() => onOpenSection?.("models")}
          onOpenModes={() => onOpenSection?.("modes")}
          onOpenPrompts={() => onOpenSection?.("settings", { tab: "prompts" })}
        />
      </TabsContent>
      <TabsContent value="prompts">
        <PromptLibrary />
      </TabsContent>
      {tabs.includes("debug") ? (
        <TabsContent value="debug">
          <DebugSettings />
        </TabsContent>
      ) : null}
    </Tabs>
  );
};
