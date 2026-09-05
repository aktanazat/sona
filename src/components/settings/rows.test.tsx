import { describe, expect, test } from "bun:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { TooltipProvider } from "@/components/vg/tooltip";
import {
  FactChip,
  Notice,
  RowReset,
  SettingsField,
  SettingsRow,
  SettingsSection,
} from "./rows";

/* Every window root mounts one `TooltipProvider` and these primitives assume
 * it, so a test that paints one has to stand in for the root. The provider is
 * context only — it adds nothing to the markup being asserted. */
const paint = (node: React.ReactElement): string =>
  renderToStaticMarkup(<TooltipProvider>{node}</TooltipProvider>);

/* The grammar itself, pinned.
 *
 * Every settings surface in the app is written in these five components, so
 * the rule the user actually cares about — a setting is stated once, and a
 * helper sentence is never printed next to the thing it restates — has to be
 * enforced here rather than re-argued on eighty pages. A `hint` that leaked
 * into the row's markup would put every one of those pages back to the layout
 * that was rejected, and it would do it silently. */

describe("a settings row", () => {
  test("states its setting once and puts the control flush right", () => {
    const markup = paint(
      <SettingsRow label="Push to talk">
        <button type="button">switch</button>
      </SettingsRow>,
    );

    expect(markup.split("Push to talk").length - 1).toBe(1);
    expect(markup).toContain("justify-between");
    // The label precedes the control, so the row reads left to right.
    expect(markup.indexOf("Push to talk")).toBeLessThan(
      markup.indexOf("switch"),
    );
  });

  test("a hint never reaches the row: it is a tooltip or it is nothing", () => {
    const markup = paint(
      <SettingsRow
        label="Show tray icon"
        hint="Hiding it leaves the app reachable only by its shortcut."
      />,
    );

    expect(markup).not.toContain("Hiding it leaves");
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

  test("a disabled row dims its type rather than its opacity", () => {
    const markup = paint(<SettingsRow label="Output device" disabled />);

    expect(markup).toContain("text-gray-700");
    expect(markup).not.toContain("opacity-");
    expect(markup).toContain('data-disabled="true"');
  });

  test("a measured value rides beside the label as a tabular fact, not a sentence", () => {
    const markup = paint(<SettingsRow label="Volume" fact="60%" />);

    expect(markup).toContain("tabular-nums");
    expect(markup.split("60%").length - 1).toBe(1);
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

describe("a settings section", () => {
  test("is one surface with hairlines, not a stack of boxes", () => {
    const markup = paint(
      <SettingsSection label="Shortcuts">
        <SettingsRow label="Transcribe" />
        <SettingsRow label="Cancel" />
      </SettingsSection>,
    );

    expect(markup.split("rounded-card").length - 1).toBe(1);
    expect(markup).toContain("divide-y");
    expect(markup).toContain("bg-surface-raised");
    // A section at rest casts nothing.
    expect(markup).not.toContain("shadow-");
  });

  test("labels itself once, a step quieter than the rows under it", () => {
    const markup = paint(
      <SettingsSection label="Appearance">
        <SettingsRow label="Theme" />
      </SettingsSection>,
    );
    const heading = markup.match(/<h2[^>]*>/)?.[0] ?? "";

    expect(markup.split("Appearance").length - 1).toBe(1);
    /* The label is meta type over a body-type row. A section label set at the
     * row's own size is the "every line looks like body text" failure the
     * round-6 pages were redesigned out of, and the old assertion could not
     * see it: it matched the body size anywhere in the markup, which the row
     * inside the section supplies on its own. */
    expect(heading).toContain("text-[13px]");
    expect(heading).toContain("text-gray-900");
    expect(markup).toContain("text-[14px]");
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

    expect(markup).not.toContain("Applied after transcription");
    expect(markup).toContain('for="replacements"');
    expect(markup.indexOf("Replacements")).toBeLessThan(
      markup.indexOf("<textarea"),
    );
  });
});

describe("a measurement chip", () => {
  test("prints its name and its value once each, tabular", () => {
    const markup = paint(<FactChip label="DURATION" value="12:04" />);

    expect(markup.split("DURATION").length - 1).toBe(1);
    expect(markup.split("12:04").length - 1).toBe(1);
    expect(markup).toContain("tabular-nums");
  });

  test("is meta type and wears no box, since nothing here can be pressed", () => {
    const markup = paint(<FactChip label="Duration" value="12:04" />);

    expect(markup).toContain("text-[13px]");
    expect(markup).not.toContain("text-[14px]");
    expect(markup).not.toContain("border");
    expect(markup).not.toContain("rounded");
  });
});

describe("a notice", () => {
  test("is an announced sentence, not a panel", () => {
    const markup = paint(<Notice tone="danger">Could not save.</Notice>);

    expect(markup).toContain('role="status"');
    expect(markup).toContain('aria-live="polite"');
    expect(markup).toContain("text-red-900");
    expect(markup).not.toContain("rounded-card");
    expect(markup).not.toContain("border");
  });

  test("can stay silent when the surface announces the change itself", () => {
    const markup = paint(<Notice live={false}>Saved 12:04</Notice>);

    expect(markup).not.toContain("aria-live");
    expect(markup).not.toContain('role="status"');
  });
});
