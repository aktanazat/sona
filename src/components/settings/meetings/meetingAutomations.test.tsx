import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import { MeetingAutomations } from "./MeetingAutomations";

/* D22's settings surface, at first paint.
 *
 * Static rendering runs no effects, so no Tauri command is reachable from here:
 * what is pinned is the copy that has to be on screen before anybody presses
 * anything — the sentence that says these actions stay on this Mac, and the
 * hint that says a webhook is refused outside the operator's own network.
 *
 * The 24-locale parity check is here rather than in a lint because the failure
 * it catches is a shipped English string on a Japanese settings page. */

const localeRoot = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "i18n",
  "locales",
);

/* The slice of a locale file this test reads, declared where the file is
 * parsed rather than sniffed at each assertion. A locale nobody has translated
 * yet is missing the block entirely, which is one of the states the parity
 * check below reports; a locale that holds something other than a string under
 * one of its keys fails the same check when `.trim()` finds no string there. */
interface AutomationsCatalogue {
  settings?: { meetings?: { automations?: Record<string, string> } };
}

const translation = (locale: string): AutomationsCatalogue =>
  JSON.parse(
    fs.readFileSync(path.join(localeRoot, locale, "translation.json"), "utf8"),
  );

const automations = (locale: string): Record<string, string> | undefined =>
  translation(locale).settings?.meetings?.automations;

/* The English block is the expectation every other locale is measured against,
 * and it is checked in beside this test, so its absence is a broken fixture
 * rather than a finding. */
const english = automations("en");
if (english === undefined) throw new Error("en has no automations block");

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: translation("en") } },
  interpolation: { escapeValue: false },
});

const render = (node: React.ReactElement) =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

describe("meeting automations settings", () => {
  test("first paint states what the section is before it has read anything", () => {
    const markup = render(<MeetingAutomations />);

    expect(markup).toContain("After a meeting");
    expect(markup).toContain("Reading your series…");
    // Nothing that would run is named until the roster has been read.
    expect(markup).not.toContain("Run a Shortcut");
    expect(markup).not.toContain("Send to a webhook");
  });

  test("the section promises the actions stay on this Mac", () => {
    expect(english.description).toContain("on this Mac");
    expect(english.webhookHint).toContain("tailnet");
    expect(english.remindersHint).toContain("Nothing is ever read back");
  });

  test("every locale carries the same automation keys, all non-empty", () => {
    const locales = fs
      .readdirSync(localeRoot)
      .filter((entry) =>
        fs.statSync(path.join(localeRoot, entry)).isDirectory(),
      );
    const expected = Object.keys(english).sort();
    /* Collected rather than asserted per locale: a failure has to name which
     * locale drifted, and bun's `expect` carries no message argument. */
    const wrong: string[] = [];

    for (const locale of locales) {
      const block = automations(locale);
      if (block === undefined) {
        wrong.push(`${locale}: no automations block`);
        continue;
      }
      const keys = Object.keys(block).sort();
      if (JSON.stringify(keys) !== JSON.stringify(expected)) {
        wrong.push(`${locale}: keys ${keys.join(",")}`);
      }
      for (const [key, value] of Object.entries(block)) {
        if (value.trim() === "") wrong.push(`${locale}.${key}: empty`);
      }
    }

    expect(locales.length).toBe(24);
    expect(wrong).toEqual([]);
    expect(expected.length).toBe(23);
  });

  test("the count and date placeholders survive translation", () => {
    const locales = fs
      .readdirSync(localeRoot)
      .filter((entry) =>
        fs.statSync(path.join(localeRoot, entry)).isDirectory(),
      );
    const wrong = locales.filter((locale) => {
      const fact = automations(locale)?.seriesFact ?? "";
      return !fact.includes("{{count}}") || !fact.includes("{{when}}");
    });

    expect(wrong).toEqual([]);
  });
});
