import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { PromptRun, PromptRunResult } from "@/bindings";
import { TooltipProvider } from "@/components/vg/tooltip";
import { PromptRunBody } from "./review/PromptResults";

const localeRoot = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "i18n",
  "locales",
);

const english = JSON.parse(
  fs.readFileSync(path.join(localeRoot, "en", "translation.json"), "utf8"),
);

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: english } },
  interpolation: { escapeValue: false },
});

const render = (node: React.ReactElement) =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

const run = (result: PromptRunResult): PromptRun => ({
  run_id: "11111111-1111-4111-8111-111111111111",
  prompt_id: "22222222-2222-4222-8222-222222222222",
  target_kind: "meeting",
  target_id: "33333333-3333-4333-8333-333333333333",
  artifact_id: null,
  model_id: "stub",
  model_version: "1",
  produced_at_utc_ms: 1_700_000_000_000,
  result,
});

describe("saved prompts", () => {
  test("a text answer is rendered as prose", () => {
    const markup = render(
      <PromptRunBody run={run({ kind: "text", text: "Ship on **Friday**" })} />,
    );

    expect(markup).toContain("Ship on ");
    expect(markup).toContain("<strong>Friday</strong>");
  });

  test("a schema answer is rendered as key and value rows", () => {
    const markup = render(
      <PromptRunBody
        run={run({
          kind: "json",
          json: '{"decisions":["Ship on Friday","Hold pricing"],"confident":true}',
        })}
      />,
    );

    expect(markup).toContain("decisions");
    expect(markup).toContain("Ship on Friday, Hold pricing");
    expect(markup).toContain("confident");
    expect(markup).toContain("true");
  });
});
