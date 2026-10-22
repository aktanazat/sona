import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import type { AppSettings } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { SettingsHub } from "./SettingsHub";

const catalogue = JSON.parse(
  fs.readFileSync(
    path.join(
      path.dirname(fileURLToPath(import.meta.url)),
      "..",
      "..",
      "i18n",
      "locales",
      "en",
      "translation.json",
    ),
    "utf8",
  ),
);

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: catalogue } },
  interpolation: { escapeValue: false },
  /* A key with no entry comes back as this sentinel rather than as itself, so
   * a page that prints one is distinguishable from a page whose copy happens
   * to read like a key. Shared with every sibling suite in this directory. */
  parseMissingKeyHandler: () => "__MISSING__",
});

/* The values the backend hands back on a fresh install (settings.rs), because
 * an unread store paints a surface nobody sees: it shows the cancel chord that
 * push-to-talk removes. Only the keys that decide whether a row renders at all
 * are listed. */
/* SAFETY: a four-key partial stands in for AppSettings because these renders
 * read only the keys that decide whether a row appears; every other field is
 * behind `getSetting` fallbacks, so a missing key cannot be dereferenced. */
const SHIPPED_DEFAULTS = {
  push_to_talk: true,
  command_mode_enabled: true,
  experimental_enabled: false,
  overlay_style: "live",
} as AppSettings;

const paint = (
  node: React.ReactElement,
  settings: AppSettings = SHIPPED_DEFAULTS,
): string => {
  useSettingsStore.setState({ settings, isUpdating: {} });
  return renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );
};
const activeTab = (markup: string): string => {
  const start = markup.indexOf('data-state="active"');
  return markup.slice(start, markup.indexOf("</button>", start));
};

const priorWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
beforeAll(() => {
  /* `type()` is a synchronous read of a global the Tauri host installs, and two
   * surfaces branch on it during render: the cancel-chord row and the line that
   * names the debug chord. defineProperty + afterAll restore, not assignment: a
   * leaked bare `window` pins framer-motion's reduced-motion probe for every
   * later test file (see Overview.test.tsx). */
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      ...globalThis.window,
      __TAURI_OS_PLUGIN_INTERNALS__: { os_type: "macos" },
    },
  });
});
afterAll(() => {
  if (priorWindow) Object.defineProperty(globalThis, "window", priorWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

describe("settings navigation requests", () => {
  test("plain Settings opens Essentials", () => {
    const found = paint(<SettingsHub />);

    expect(activeTab(found)).toContain("Essentials");
  });

  test("the agent target selects Advanced and reveals pairing", () => {
    const found = paint(
      <SettingsHub
        navigationRequest={{
          target: { tab: "advanced", section: "sonaAgent" },
          nonce: 4,
        }}
      />,
    );

    expect(activeTab(found)).toContain("Advanced");
    const openDetailsStart = found.indexOf('<details open=""');
    const openDetails = found.slice(
      openDetailsStart,
      found.indexOf("</details>", openDetailsStart),
    );
    expect(openDetails).toMatch(/<summary[^>]*>Sona agent/);
  });

});

