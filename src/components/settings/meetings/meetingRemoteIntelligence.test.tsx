import { afterAll, beforeEach, describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { type AppSettings } from "@/bindings";
import { TooltipProvider } from "@/components/vg/tooltip";
import { useSettingsStore } from "@/stores/settingsStore";
import { MeetingRemoteIntelligence } from "./MeetingRemoteIntelligence";

/* D14's first paint, on a Mac that has never been paired with a server — which
 * is every Mac on install, and the state this section has to be honest in.
 *
 * Static rendering runs no effects, so no command is reachable from here: what
 * is checked is the consent surface itself. The consent sentence has to be on
 * screen rather than behind an info affordance, because it names what leaves
 * the machine, and the switch has to be refused while there is no server to
 * route anything to. */

const localeRoot = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localeRoot, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

const paint = () =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>
        <MeetingRemoteIntelligence />
      </TooltipProvider>
    </I18nextProvider>,
  );

describe("meeting intelligence", () => {
  beforeEach(() => {
    // SAFETY: The component reads only meeting_local_engine from this fixture.
    const defaultSettings = {
      meeting_local_engine: { kind: "apple_intelligence" },
    } as AppSettings;
    useSettingsStore.setState({ settings: null, defaultSettings });
  });

  afterAll(() => {
    useSettingsStore.setState({
      settings: null,
      defaultSettings: null,
    });
  });

  test("uses the backend default endpoint when no local setting is saved", () => {
    // SAFETY: The component reads only meeting_local_engine from this fixture.
    const defaultSettings = {
      meeting_local_engine: {
        kind: "local_endpoint",
        base_url: "http://127.0.0.1:9999/v1",
        model: "backend-default-model",
        context_window_tokens: 4096,
      },
    } as AppSettings;
    useSettingsStore.setState({ settings: null, defaultSettings });
    const markup = paint();

    expect(markup).toContain("http://127.0.0.1:9999/v1");
    expect(markup).toContain("backend-default-model");
  });

  test("names the engine picker by its row label, for assistive tech", () => {
    const markup = paint();
    const label = i18n.t("settings.meetings.localEngine.label");
    const engineId = new RegExp(
      `<label for="([^"]+)"[^>]*>${label}</label>`,
    ).exec(markup)?.[1];

    expect(engineId).toBeDefined();
    expect(markup).toContain(`role="combobox"`);
    expect(markup).toContain(`id="${engineId ?? "?"}"`);
  });

  test("names exactly what leaves the Mac, on the surface", () => {
    expect(paint()).toContain(
      i18n.t("settings.meetings.remoteIntelligence.consent"),
    );
  });

  test("shows the local engine choice and its availability line", () => {
    const markup = paint();

    expect(markup).toContain(i18n.t("settings.meetings.localEngine.label"));
    expect(markup).toContain(
      i18n.t("settings.meetings.localEngine.status.checking"),
    );
  });

  test("cannot be turned on before a server is paired", () => {
    const markup = paint();

    expect(markup).toContain(
      i18n.t("settings.meetings.remoteIntelligence.unpaired"),
    );
    expect(markup).toMatch(/disabled/);
  });

  test("offers no per-series list while it is off", () => {
    const markup = paint();

    expect(markup).not.toContain(
      i18n.t("settings.meetings.remoteIntelligence.seriesTitle"),
    );
  });
});
