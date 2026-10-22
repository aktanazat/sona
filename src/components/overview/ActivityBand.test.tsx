import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { HistoryTrendProjection } from "@/bindings";
import { TooltipProvider } from "@/components/vg/tooltip";
import { ActivityBand } from "./ActivityBand";

const localeRoot = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
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

const render = (node: React.ReactElement): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

const occurrences = (markup: string, needle: string): number =>
  markup.split(needle).length - 1;


const trend: HistoryTrendProjection = {
  range: "days_180",
  range_start_local_date: "2026-03-04",
  range_end_local_date: "2026-08-30",
  all_time: { recordings: 18, duration_ms: 12_000, words: 180, by_source: [] },
  range_total: {
    recordings: 18,
    duration_ms: 12_000,
    words: 180,
    by_source: [],
  },
  active_days: 6,
  current_streak_days: 3,
  points: [
    {
      local_date: "2026-08-24",
      recordings: 1,
      duration_ms: 1000,
      words: 10,
      by_source: [],
    },
    {
      local_date: "2026-08-25",
      recordings: 4,
      duration_ms: 2000,
      words: 40,
      by_source: [],
    },
    {
      local_date: "2026-08-26",
      recordings: 2,
      duration_ms: 1000,
      words: 20,
      by_source: [],
    },
    {
      local_date: "2026-08-27",
      recordings: 0,
      duration_ms: 0,
      words: 0,
      by_source: [],
    },
    {
      local_date: "2026-08-28",
      recordings: 6,
      duration_ms: 4000,
      words: 60,
      by_source: [],
    },
    {
      local_date: "2026-08-29",
      recordings: 3,
      duration_ms: 2000,
      words: 30,
      by_source: [],
    },
    {
      local_date: "2026-08-30",
      recordings: 2,
      duration_ms: 2000,
      words: 20,
      by_source: [],
    },
  ],
};

describe("ActivityBand charts", () => {
  test("gives each activity chart an accessible summary of its data", () => {
    const markup = render(<ActivityBand trend={trend} />);

    expect(occurrences(markup, 'role="img"')).toBe(3);
    expect(markup).toContain(
      'aria-label="Dictations per day, highest 6 on Friday"',
    );
    expect(markup).toContain(
      'aria-label="Words per day, 180 total, ending at 20"',
    );
    expect(markup).toContain(
      'aria-label="Current streak, 3 days. Active days this week:',
    );
  });

});
