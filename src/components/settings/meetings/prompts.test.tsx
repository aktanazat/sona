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
import { MeetingPrompts } from "./MeetingPrompts";
import { PromptRunBody } from "./review/PromptResults";
import {
  promptFailureKeys,
  promptTargetRef,
  usePromptShellStore,
} from "./promptTargets";

/* Saved prompts, at the three moments a reader meets them: the list before it
 * has read anything, an answer in each of its three shapes, and the ref a
 * surface hands the store.
 *
 * Static rendering runs no effects, so no Tauri command is reachable from here.
 * What is pinned is what has to be on screen before anybody presses anything,
 * and the copy that stands in for an answer that never arrived — a failed run
 * is never retried, so its sentence is the whole record. */

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
  test("the list says what it is before it has read anything", () => {
    const markup = render(
      <MeetingPrompts createRequest={null} onCreateRequestHandled={() => {}} />,
    );

    expect(markup).toContain("Prompts");
    expect(markup).toContain("Reading your prompts…");
    // No editor until somebody asks for one.
    expect(markup).not.toContain("Schema");
  });

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

  test("a failed run says which absence it was, not that nothing happened", () => {
    const markup = render(
      <PromptRunBody
        run={run({ kind: "failed", reason: "schema_mismatch" })}
      />,
    );

    expect(markup).toContain("did not match the schema");
  });

  test("every failure has a sentence of its own", () => {
    const missing = Object.values(promptFailureKeys).filter(
      (key) => i18n.t(key) === key,
    );

    expect(missing).toEqual([]);
  });

  test("a target ref names the noun the way the store keys it", () => {
    expect(promptTargetRef("meeting", "m-1")).toEqual({
      kind: "meeting",
      session_id: "m-1",
    });
    expect(promptTargetRef("person", "p-1")).toEqual({
      kind: "person",
      person_id: "p-1",
    });
    expect(promptTargetRef("series", "weekly")).toEqual({
      kind: "series",
      series_key: "weekly",
    });
  });
  test("each create request is consumed once", () => {
    usePromptShellStore.setState({
      newPromptRequest: 0,
      handledNewPromptRequest: 0,
    });
    usePromptShellStore.getState().requestNewPrompt();
    const first = usePromptShellStore.getState().newPromptRequest;

    expect(usePromptShellStore.getState().consumeNewPromptRequest(first)).toBe(
      true,
    );
    expect(usePromptShellStore.getState().consumeNewPromptRequest(first)).toBe(
      false,
    );

    usePromptShellStore.getState().requestNewPrompt();
    const second = usePromptShellStore.getState().newPromptRequest;
    expect(second).toBe(first + 1);
    expect(usePromptShellStore.getState().consumeNewPromptRequest(second)).toBe(
      true,
    );
  });
});
