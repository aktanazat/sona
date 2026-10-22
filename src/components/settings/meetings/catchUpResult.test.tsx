import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { CatchUpResult } from "./MeetingNotesPane";
import type { MeetingCatchUp } from "./meetingAnalytics";

/* The recap panel, mid-meeting and afterwards.
 *
 * A recap of a conversation that is still happening is only true up to a
 * moment, so the panel has to say which moment and that the words behind it
 * are provisional. It also must not put transcript on screen: this app shows a
 * recap during a meeting, never a live transcript. */

const localePath = path.join(
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
    en: { translation: JSON.parse(fs.readFileSync(localePath, "utf8")) },
  },
  interpolation: { escapeValue: false },
  parseMissingKeyHandler: () => "__MISSING__",
});

const paint = (result: MeetingCatchUp) =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <CatchUpResult result={result} />
    </I18nextProvider>,
  );

const READY: MeetingCatchUp = {
  state: "ready",
  bullets: ["Pricing stayed open."],
  through_offset_ns: 754_000_000_000,
  segment_count: 12,
  provisional: true,
};

describe("catch-up result", () => {
  test("stamps a provisional recap with how far into the meeting it read", () => {
    const markup = paint(READY);

    expect(markup).toContain("Pricing stayed open.");
    expect(markup).toContain("As of 12:34, provisional");
  });

  test("says nothing about provenance once the transcript is stored", () => {
    const markup = paint({
      ...READY,
      provisional: false,
      through_offset_ns: null,
    });

    expect(markup).toContain("Pricing stayed open.");
    expect(markup).not.toContain("provisional");
  });
});
