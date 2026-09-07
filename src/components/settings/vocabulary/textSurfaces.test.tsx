import { describe, expect, test } from "bun:test";
import { z } from "zod";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import { CustomWords } from "../CustomWords";
import { DictationPrompts } from "./PromptLibrary";

/**
 * Mount checks for the two text-rule surfaces.
 *
 * The i18n instance carries no resources on purpose: every string falls back
 * to the inline English default, so these assertions stay true after
 * integration harvests the new keys into the locale files.
 *
 * A static render runs the component bodies without effects, which is exactly
 * the pre-data state: nothing here reaches Tauri. The tooltip provider is the
 * one piece of the route root a settings surface needs, because a row hint is
 * a Radix tooltip and Radix refuses to render one outside a provider.
 */
const render = async (node: React.ReactElement): Promise<string> => {
  const i18n = createInstance();
  await i18n.init({
    lng: "en",
    fallbackLng: "en",
    resources: { en: { translation: {} } },
    interpolation: { escapeValue: false },
  });
  return renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );
};

describe("DictationPrompts", () => {
  test("mounts its loading list without a second create action", async () => {
    const markup = await render(
      <DictationPrompts
        createRequest={null}
        onCreateRequestHandled={() => {}}
      />,
    );

    expect(markup).toContain("Post-processing prompts");
    expect(markup).toContain('aria-label="Loading prompts"');
    expect(markup).toContain('data-testid="dictation-prompt-list"');
    expect(markup).not.toContain('data-testid="prompt-create"');
  });
});

describe("CustomWords", () => {
  test("puts every kind of text rule in one list with one add flow", async () => {
    /* Real copy: the merged list's strings live only in the locale file, so
     * the empty-resource render would prove a key path rather than a label. */
    const markup = await renderWithEnglishCopy(<CustomWords />);

    expect(markup).toContain('data-testid="rules-editor"');
    expect(markup).toContain('data-testid="rule-new-kind"');
    expect(markup).toContain('data-testid="rule-new-left"');
    expect(markup).toContain('data-testid="rule-new-right"');
    expect(markup).toContain('data-testid="rule-add"');
    // One add flow, so there is no second one for any other kind.
    expect(markup.match(/data-testid="rule-add"/g)?.length).toBe(1);
    // Rows only exist once the stores answer.
    expect(markup.includes('data-testid="rule-row"')).toBe(false);
    expect(markup).toContain('aria-label="Loading text rules"');
  });

  test("keeps the four master switches above the list", async () => {
    const markup = await renderWithEnglishCopy(<CustomWords />);

    expect(markup.match(/role="switch"/g)?.length).toBe(4);
    for (const label of [
      "Obey spoken editing commands",
      "Write emoji when you name one",
      "Expand shortcuts",
      "Apply rewrites",
    ]) {
      expect(markup).toContain(label);
    }
  });

  test("keeps the vocabulary CSV actions and the rewrite reset reachable", async () => {
    const markup = await render(<CustomWords />);

    expect(markup).toContain('role="group"');
    expect(markup).toContain('accept=".csv,text/csv"');
    expect(markup).toContain('data-testid="rules-restore-rewrites"');
  });
});

/**
 * A locale catalogue as i18next reads it: a sentence, or a group of them.
 * Stated as a schema so a nested object where a sentence belongs, or a number
 * that slipped into a translation file, fails at the read instead of rendering
 * as `[object Object]` in an assertion nobody re-checks.
 */
type LocaleNode = string | { [key: string]: LocaleNode };

const localeCatalogue: z.ZodType<LocaleNode> = z.lazy(() =>
  z.union([z.string(), z.record(localeCatalogue)]),
);

/* The root of a bundle is always an object of namespaces; parsing it as one
 * types every lookup below without an assertion. */
const localeRecord = z.record(localeCatalogue);

const englishCopy = localeRecord.parse(
  JSON.parse(
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
);

/**
 * The rows whose copy lives only in the locale files render with the real
 * English resources instead, so the assertion proves the key is wired rather
 * than that an inline default exists.
 */
const renderWithEnglishCopy = async (
  node: React.ReactElement,
): Promise<string> => {
  const i18n = createInstance();
  await i18n.init({
    lng: "en",
    fallbackLng: "en",
    resources: { en: { translation: englishCopy } },
    interpolation: { escapeValue: false },
  });
  return renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );
};

/* Each step re-parses the node as a namespace record, so a path that dives
 * through a string — or a key that is missing — fails AT the read with zod's
 * own error naming the shape, instead of rendering `[object Object]`. */
const englishSentence = (dottedPath: string): string => {
  let node: LocaleNode = englishCopy;
  for (const key of dottedPath.split(".")) {
    node = localeRecord.parse(node)[key] ?? "";
  }
  return z.string().parse(node);
};

describe("spoken editing commands row", () => {
  test("states which phrases fire and which stay as text", async () => {
    const limits = englishSentence(
      "settings.advanced.spokenEdits.enabledDescription",
    );

    expect(limits).toContain("delete the last word");
    // The row names its own limits rather than promising general editing.
    expect(limits).toContain("English only");
    expect(limits).toContain("scratch that plan");
  });

  test("carries those limits on an affordance, not as a second sentence", async () => {
    const markup = await renderWithEnglishCopy(<CustomWords />);

    expect(markup).toContain("Obey spoken editing commands");
    // The hint is reachable and named, and its text is not printed under the
    // label: a row states its setting once.
    expect(markup).toContain('aria-label="Obey spoken editing commands"');
    expect(markup).toContain('data-slot="tooltip-trigger"');
    expect(markup.includes("English only")).toBe(false);
  });
});
