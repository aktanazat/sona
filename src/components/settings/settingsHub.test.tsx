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
import { useModelStore } from "@/stores/modelStore";
import { EssentialsSettings } from "./essentials/EssentialsSettings";
import { AdvancedSettings } from "./advanced/AdvancedSettings";
import { SettingsHub } from "./SettingsHub";
import { ModelsSettings } from "./models/ModelsSettings";
import { nextSettingsNavigationRequest } from "./navigation";
import { PromptLibrary } from "./prompts/PromptLibrary";

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

describe("Essentials", () => {
  const markup = () => paint(<EssentialsSettings />);

  test("carries every essential control, in the brief's order", () => {
    const found = markup();
    const order = [
      // The transcribe binding, before its store has resolved.
      "Shortcut",
      "Push to talk",
      "Microphone",
      "Language",
      "Sounds",
      "Launch at login",
      "Notice when I join a meeting",
      "Meeting apps",
    ].map((label) => found.indexOf(label));

    // Every one present, and each after the one before it.
    expect(order.filter((at) => at < 0)).toEqual([]);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });

  /* Meeting apps is the one list here a reader has to open: six checkboxes,
   * two switches and an Add button, behind a single line. Either this list
   * flattening back onto the page or a second block folding away changes what
   * Essentials costs to read. */
  test("keeps the meeting-app allowlist behind a single disclosure", () => {
    expect(markup().match(/<summary/g)?.length).toBe(1);
  });

  /* A row wired to a key the catalogue does not carry prints that key to the
   * reader. Each sibling suite checks its own component; this file is the only
   * one that paints a whole composed page, which is where a row added on one
   * side and a string added on the other drift apart. */
  test("resolves every string it renders", () => {
    expect(markup()).not.toContain("__MISSING__");
  });
});

describe("Advanced", () => {
  const markup = () =>
    paint(
      <AdvancedSettings
        onOpenCatalog={() => {}}
        onOpenModes={() => {}}
        onOpenPrompts={() => {}}
      />,
    );

  test("names the chord that opens Debug, since nothing links to it", () => {
    expect(markup()).toContain("Press \u2318\u21e7D to open the debug page.");
  });

  test("every setting with a live reader keeps a way to write it", () => {
    /* The row cull orphaned five settings: it deleted their components while
     * Rust kept reading the fields. `app_language` decides the whole UI's
     * locale and had no control left at all, in the same change that synced 23
     * locale bundles; the HUD pill, the window material and the microphone
     * channel each still drive real behaviour. Named by label because that is
     * what a reader looks for. */
    const found = markup();

    for (const label of ["App language", "Material", "Show the idle pill"]) {
      expect(found).toContain(label);
    }
    /* The channel row is absent by design: `ChannelSelector` asks the device
     * how many channels it has and renders nothing for the ordinary one, so a
     * static render — which runs no effects — must show no row. */
    expect(found).not.toContain("Input channel");
  });

  /* Five tabs' worth of rows fold into this one page, so a key missing from
   * any of them reaches a reader here first. */
  test("resolves every string it renders", () => {
    expect(markup()).not.toContain("__MISSING__");
  });
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

  test("repeating one destination still creates a new request", () => {
    const target = { tab: "advanced", section: "meetings" } as const;
    const first = nextSettingsNavigationRequest(null, target);
    const second = nextSettingsNavigationRequest(first, target);

    expect(first).toEqual({ target, nonce: 1 });
    expect(second).toEqual({ target, nonce: 2 });
  });
});

describe("the single prompt library", () => {
  test("owns both prompt stores and one create action", () => {
    const found = paint(<PromptLibrary />);

    expect(found).toContain("Prompts");
    expect(found).toContain(">Meetings</button>");
    expect(found).toContain(">Dictation</button>");
    expect(found.match(/data-testid="prompt-create"/g) ?? []).toHaveLength(1);
    expect(found.match(/data-testid="prompt-library"/g) ?? []).toHaveLength(1);
  });

  test("Advanced links to prompts without mounting an editor", () => {
    const found = paint(
      <AdvancedSettings
        onOpenCatalog={() => {}}
        onOpenModes={() => {}}
        onOpenPrompts={() => {}}
      />,
    );

    expect(found).toContain("Prompts");
    expect(found).toContain("Open");
    expect(found).not.toContain('data-testid="prompt-library"');
    expect(found).not.toContain("Reading your prompts ");
  });

  test("Models links to prompts without mounting an editor", () => {
    useModelStore.setState({ loading: false, models: [] });
    const found = paint(<ModelsSettings onOpenPrompts={() => {}} />);

    expect(found).toContain("Post-processing prompts");
    expect(found).toContain("Open");
    expect(found).not.toContain('data-testid="prompt-library"');
  });
});
