import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useSettings } from "@/hooks/useSettings";
import { cn } from "@/lib/cn";
import { PAGE_COLUMN } from "./rows";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/vg/tabs";
import { EssentialsSettings } from "./essentials/EssentialsSettings";
import { AdvancedSettings } from "./advanced/AdvancedSettings";
import { DebugSettings } from "./debug/DebugSettings";

type SettingsTab = "essentials" | "advanced" | "debug";

const BASE_TABS: readonly SettingsTab[] = ["essentials", "advanced"];

/* Two tabs, because there are two questions: what do I want changed, and what
 * is Sona allowed to do. General, Privacy, Agents, Workflows and About were
 * five answers to the second one.
 *
 * Debug is a third tab only on a build the chord has unlocked, which is why it
 * is appended rather than declared: it is instrumentation, and a person who has
 * not asked for it should not be able to see that it exists.
 *
 * The strip is a hairline the width of the window with an underline mark under
 * the active tab. Its labels sit on the same 760px column as the page below, so
 * the first tab and the page title share a left edge. The mark is the kit's own
 * `line` variant, not a hand-rolled bottom border: that variant parks its bar
 * one pixel under the list, which is exactly where this container's hairline
 * is, so the mark reads as a break in the rule. Pulled to a single pixel in the
 * accent, because a 2px bar in the text colour is a second heading; the active
 * label carries the weight instead. */
const TAB_TRIGGER =
  "flex-none px-0 text-[14px] leading-[21px] font-normal text-gray-900 transition-colors hover:text-gray-1000 focus-visible:border-transparent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none motion-reduce:transition-none data-[state=active]:font-medium data-[state=active]:text-gray-1000 after:bg-primary group-data-[orientation=horizontal]/tabs:after:h-px";

export const SettingsHub: React.FC<{
  /**
   * The shell's section setter, for the two editors Essentials and Advanced
   * hand off to. Optional for the same reason every other section's shell
   * callback is: these pages render standalone in tests, where there is no
   * shell to route with, and the registry in `sidebarSections` types every
   * destination as a bare component.
   */
  onOpenSection?: (section: "modes" | "models") => void;
}> = ({ onOpenSection }) => {
  const { t } = useTranslation();
  const { settings } = useSettings();
  const [activeTab, setActiveTab] = useState<SettingsTab>("essentials");
  const tabs = useMemo(
    () => (settings?.debug_mode ? [...BASE_TABS, "debug" as const] : BASE_TABS),
    [settings?.debug_mode],
  );
  const visibleTab = tabs.includes(activeTab) ? activeTab : "essentials";

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
              {t(`settingsV2.tabs.${tab}`)}
            </TabsTrigger>
          ))}
        </TabsList>
      </div>
      <TabsContent value="essentials">
        <EssentialsSettings />
      </TabsContent>
      <TabsContent value="advanced">
        <AdvancedSettings
          onOpenCatalog={() => onOpenSection?.("models")}
          onOpenModes={() => onOpenSection?.("modes")}
        />
      </TabsContent>
      {tabs.includes("debug") ? (
        <TabsContent value="debug">
          <DebugSettings />
        </TabsContent>
      ) : null}
    </Tabs>
  );
};
