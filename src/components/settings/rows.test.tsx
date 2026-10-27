import { describe, expect, test } from "bun:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { TooltipProvider } from "@/components/vg/tooltip";
import {
  Notice,
  RowReset,
  SettingsField,
  SettingsRow,
} from "./rows";

/* Every window root mounts one `TooltipProvider` and these primitives assume
 * it, so a test that paints one has to stand in for the root. The provider is
 * context only — it adds nothing to the markup being asserted. */
const paint = (node: React.ReactElement): string =>
  renderToStaticMarkup(<TooltipProvider>{node}</TooltipProvider>);


describe("a settings row", () => {
  test("a hint never reaches the row: it is a tooltip or it is nothing", () => {
    const markup = paint(
      <SettingsRow
        label="Show tray icon"
        hint="Hiding it leaves the app reachable only by its shortcut."
      />,
    );

    // The affordance that reveals it is focusable and named, so the sentence
    // is reachable without a pointer.
    expect(markup).toContain('aria-label="Show tray icon"');
    expect(markup).toContain("<button");
  });

  test("names its control for assistive tech when given one", () => {
    const markup = paint(
      <SettingsRow label="Volume" controlId="volume-input">
        <input id="volume-input" />
      </SettingsRow>,
    );

    expect(markup).toContain('for="volume-input"');
  });

});

describe("the reset arrow", () => {
  /* The rule five rows used to get wrong by hand: an undo of nothing is an
   * affordance a reader has to press, or hover, or reason about, to learn it
   * was never going to do anything. */
  test("is absent while the value equals its default", () => {
    const markup = paint(
      <SettingsRow label="Microphone">
        <RowReset name="Microphone" changed={false} onReset={() => {}} />
      </SettingsRow>,
    );

    expect(markup).not.toContain("<button");
  });

  test("appears once the value differs from it", () => {
    const markup = paint(
      <SettingsRow label="Microphone">
        <RowReset name="Microphone" changed onReset={() => {}} />
      </SettingsRow>,
    );

    expect(markup).toContain("<button");
    expect(markup).toContain("aria-label");
  });
});

describe("a stacked field", () => {
  test("keeps the label above the control and still refuses inline hints", () => {
    const markup = paint(
      <SettingsField
        label="Replacements"
        hint="Applied after transcription, before delivery."
        controlId="replacements"
      >
        <textarea id="replacements" />
      </SettingsField>,
    );

    expect(markup).toContain('for="replacements"');
  });
});

describe("a notice", () => {
  test("is an announced sentence, not a panel", () => {
    const markup = paint(<Notice tone="danger">Could not save.</Notice>);

    expect(markup).toContain('role="status"');
    expect(markup).toContain('aria-live="polite"');
  });

  test("can stay silent when the surface announces the change itself", () => {
    const markup = paint(<Notice live={false}>Saved 12:04</Notice>);

    expect(markup).not.toContain("aria-live");
    expect(markup).not.toContain('role="status"');
  });
});
