import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { ShortcutRecorderField } from "./ShortcutRecorderField";
import { keyCapParts, keyCombinationParts } from "@/lib/utils/keyboard";

/* Chord labels, capture state, and the keyboard-accessible recorder surface. */

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
  parseMissingKeyHandler: () => "__MISSING__",
});

const paint = (node: React.ReactElement) =>
  renderToStaticMarkup(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);

const countKbd = (markup: string) => markup.split("<kbd").length - 1;

describe("chord parsing", () => {
  test("one entry per physical key, left/right spelled out", () => {
    expect(keyCombinationParts("option_left+shift+space")).toEqual([
      "Left Option",
      "Shift",
      "Space",
    ]);
  });

  test("an unset chord is no keys rather than one empty key", () => {
    expect(keyCombinationParts("")).toEqual([]);
  });

  /* The compact form the Modes list, the Capture strip and the HUD share.
   * It has to stay one cap per physical key, or a chord silently becomes a
   * single wide chip and the rows it feeds start wrapping. */
  test("engraved caps on macOS, one per physical key", () => {
    expect(keyCapParts("option_left+shift+space", "macos")).toEqual([
      "⌥",
      "⇧",
      "Space",
    ]);
    expect(keyCapParts("command+ctrl+enter", "macos")).toEqual([
      "⌘",
      "⌃",
      "↩",
    ]);
  });

  test("the spelled-out modifier survives off macOS", () => {
    expect(keyCapParts("alt+shift_right+f5", "windows")).toEqual([
      "Alt",
      "Shift",
      "F5",
    ]);
  });

  test("an unset chord is no caps rather than one empty cap", () => {
    expect(keyCapParts("", "macos")).toEqual([]);
  });
});

describe("resting field", () => {
  const markup = paint(
    <ShortcutRecorderField
      chord="option_left+shift+space"
      recording={false}
      captured=""
      onStartRecording={() => undefined}
      bindingName="Transcribe"
    />,
  );

  test("renders one keycap per key, never a joined string", () => {
    expect(countKbd(markup)).toBe(3);
    expect(markup).toContain("Left Option");
    expect(markup).toContain("Space");
    expect(markup.includes("+")).toBe(false);
  });

  test("is a button, so the chord reads as clickable", () => {
    expect(markup).toContain("<button");
    expect(markup).toContain('type="button"');
  });

  test("names the recording action for its binding", () => {
    expect(markup).toContain("Record a new shortcut for Transcribe");
  });

  test("an unset binding says so instead of rendering an empty box", () => {
    const empty = paint(
      <ShortcutRecorderField
        chord=""
        recording={false}
        captured=""
        onStartRecording={() => undefined}
        bindingName="Command"
      />,
    );
    expect(countKbd(empty)).toBe(0);
    expect(empty).toContain("Not set");
  });
});

describe("recording field", () => {
  test("prompts before the first key lands", () => {
    const markup = paint(
      <ShortcutRecorderField
        chord="option_left+shift+space"
        recording
        captured=""
        onStartRecording={() => undefined}
        bindingName="Transcribe"
      />,
    );
    expect(markup).toContain("Press your keys");
    expect(countKbd(markup)).toBe(0);
  });

  test("shows the captured keys, not the committed ones", () => {
    const markup = paint(
      <ShortcutRecorderField
        chord="option_left+shift+space"
        recording
        captured="ctrl+alt"
        onStartRecording={() => undefined}
        bindingName="Transcribe"
      />,
    );
    expect(countKbd(markup)).toBe(2);
    expect(markup).toContain("Ctrl");
    expect(markup).toContain("Alt");
    expect(markup.includes("Space")).toBe(false);
  });

  test("is not a button — the parent owns commit and cancel", () => {
    const markup = paint(
      <ShortcutRecorderField
        chord="option_left+shift+space"
        recording
        captured=""
        onStartRecording={() => undefined}
        bindingName="Transcribe"
      />,
    );
    expect(markup.includes("<button")).toBe(false);
    expect(markup).toContain('aria-live="polite"');
  });
});

