import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
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
