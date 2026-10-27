import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import { VocabularyRules } from "./VocabularyRules";
import {
  ruleId,
  RULE_KINDS,
  type MergedRule,
  type VocabularyRulesState,
} from "./useVocabularyRules";

/**
 * The merged text-rule list.
 *
 * Four stores keep their own commands and their own persisted shape; what this
 * proves is the surface over them — one list, one add flow, one row shape. Which
 * store a row belongs to is a typed field on the row, so keeping a rewrite out
 * of the spelling list is the compiler's job rather than a test's.
 */

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: {
      translation: JSON.parse(
        fs.readFileSync(
          path.join(
            path.dirname(fileURLToPath(import.meta.url)),
            "..",
            "..",
            "..",
            "i18n",
            "locales",
            "en",
            "translation.json",
          ),
          "utf8",
        ),
      ),
    },
  },
  interpolation: { escapeValue: false },
});

const render = (node: React.ReactElement): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

const noop = () => undefined;

/* One rule per store, in the order the list groups them. Each carries the
 * `enabled` value its own store actually has: two of the four have a per-rule
 * switch, and the other two must not grow one. */
const RULES: MergedRule[] = [
  {
    id: ruleId("vocabulary", 0),
    address: { kind: "vocabulary", index: 0 },
    kind: "vocabulary",
    left: "open ai",
    right: "OpenAI",
    enabled: null,
  },
  {
    id: ruleId("snippet", "snip-1"),
    address: { kind: "snippet", snippetId: "snip-1" },
    kind: "snippet",
    left: "omw",
    right: "on my way",
    enabled: true,
  },
  {
    id: ruleId("replacement", 3),
    address: { kind: "replacement", index: 3 },
    kind: "replacement",
    left: "at sign",
    right: "@",
    enabled: false,
  },
  {
    id: ruleId("emoji", 1),
    address: { kind: "emoji", index: 1 },
    kind: "emoji",
    left: "smiley face",
    right: "🙂",
    enabled: null,
  },
];

const state = (
  overrides: Partial<VocabularyRulesState> = {},
): VocabularyRulesState => ({
  rules: RULES,
  loading: false,
  busy: false,
  failure: null,
  problems: {},
  savedVocabularyCount: 1,
  editRule: noop,
  commitRule: noop,
  removeRule: noop,
  toggleRule: noop,
  addRule: noop,
  addSuggestion: noop,
  vocabularyEntries: [],
  spokenEditsEnabled: false,
  emojiEnabled: false,
  snippetsEnabled: true,
  replacementsEnabled: true,
  setSpokenEdits: noop,
  setEmoji: noop,
  setSnippets: noop,
  setReplacements: noop,
  restoreDefaultRewrites: noop,
  review: null,
  previewImport: noop,
  applyImport: noop,
  exportCsv: noop,
  setReviewStep: noop,
  closeReview: noop,
  ...overrides,
});

describe("the merged rule list", () => {
  const html = render(<VocabularyRules state={state()} />);

  test("is one list holding every kind of rule", () => {
    expect(html.match(/aria-label="Text rules"/g)?.length).toBe(1);
    expect(html.match(/data-testid="rule-row"/g)?.length).toBe(4);
    for (const kind of RULE_KINDS) {
      expect(html).toContain(`data-rule-kind="${kind}"`);
    }
  });

  test("names each row's kind on the row, in words", () => {
    for (const word of ["Spelling", "Shortcut", "Rewrite", "Emoji"]) {
      expect(html).toContain(`>${word}<`);
    }
  });

  test("carries both sides of every rule, and a delete for each", () => {
    for (const [left, right] of [
      ["open ai", "OpenAI"],
      ["omw", "on my way"],
      ["at sign", "@"],
      ["smiley face", "🙂"],
    ]) {
      expect(html).toContain(`value="${left}"`);
      expect(html).toContain(`value="${right}"`);
      expect(html).toContain(`aria-label="Delete ${left}"`);
    }
  });

  /* Only two of the four stores keep a per-rule flag. A switch on a spelling
   * or an emoji row would be a control with nothing behind it. */
  test("draws a per-rule switch only where the store has one", () => {
    expect(html.match(/role="switch"/g)?.length).toBe(2);
    expect(html).toContain('aria-label="Apply omw"');
    expect(html).toContain('aria-label="Apply at sign"');
    expect(html.includes('aria-label="Apply open ai"')).toBe(false);
    expect(html.includes('aria-label="Apply smiley face"')).toBe(false);
  });

  test("has one add flow, with the kind as its first field", () => {
    expect(html.match(/data-testid="rule-add"/g)?.length).toBe(1);
    expect(html).toContain('data-testid="rule-new-kind"');
    expect(html).toContain('aria-label="Kind"');
  });

  test("names a rejected row on the row, and marks the field", () => {
    const blocked = render(
      <VocabularyRules
        state={state({
          problems: {
            [ruleId("replacement", 3)]:
              "Another rule of this kind already uses this phrase.",
          },
        })}
      />,
    );

    expect(blocked).toContain(
      "Another rule of this kind already uses this phrase.",
    );
    expect(blocked).toContain('aria-invalid="true"');
    expect(blocked).toContain(`id="rule-hint-${ruleId("replacement", 3)}"`);
  });

  test("states the two states a list has before it has rows", () => {
    expect(
      render(<VocabularyRules state={state({ loading: true })} />),
    ).toContain('aria-label="Loading text rules"');
    expect(render(<VocabularyRules state={state({ rules: [] })} />)).toContain(
      "No text rules yet",
    );
  });

  test("offers the retry beside a failed read or write", () => {
    const failed = render(
      <VocabularyRules
        state={state({ failure: { message: "store is locked", retry: noop } })}
      />,
    );

    expect(failed).toContain("store is locked");
    expect(failed).toContain("Retry");
  });
});
