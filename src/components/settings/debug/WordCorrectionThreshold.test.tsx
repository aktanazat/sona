import { afterAll, describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import type { ModelInfo } from "@/bindings";
import { useModelStore } from "@/stores/modelStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { WordCorrectionThreshold } from "./WordCorrectionThreshold";

/* This slider governs one thing: how far the fuzzy vocabulary pass may stretch
 * a word before correcting it. A whisper-family decoder never reaches that
 * pass — it is handed the vocabulary as a decode prompt, and the backend then
 * corrects exact repeats only (`vocabulary_already_prompted`,
 * managers/transcription.rs). On such a model the row used to answer the drag
 * and change nothing about the transcript, which is the reading these tests
 * exist to keep out. */

const catalogue = JSON.parse(
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
);

/* The shipped English catalogue, so copy this row names but nobody wrote
 * renders as `__MISSING__` instead of falling back silently. */
const shipped = createInstance();
void shipped.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: catalogue } },
  interpolation: { escapeValue: false },
  parseMissingKeyHandler: () => "__MISSING__",
});

const model = (id: string, name: string): ModelInfo => ({
  id,
  name,
  description: "",
  filename: "model.gguf",
  source: { HuggingFace: { repo_id: "handy-computer/x", revision: "main" } },
  size_mb: 100,
  is_downloaded: true,
  is_downloading: false,
  partial_size: 0,
  is_directory: false,
  engine_type: "TranscribeCpp",
  accuracy_score: 0.5,
  speed_score: 0.5,
  supports_translation: false,
  is_recommended: false,
  supported_languages: ["en"],
  supports_language_selection: false,
  is_custom: false,
  supports_streaming: false,
  supports_language_detection: false,
});

/* Real catalog rows. Breeze is the one prompted model whose name says nothing
 * about Whisper, and the catalog files it under the whisper architecture the
 * backend gates on. */
const CATALOG = [
  model("handy-computer/whisper-medium-gguf", "Whisper Medium"),
  model("handy-computer/Breeze-ASR-25-gguf", "Breeze ASR"),
  model("parakeet-tdt-0.6b-v3", "Parakeet V3"),
];

/* zustand hands React's *server* snapshot `getInitialState()`, so a store
 * seeded through `setState` never reaches `renderToStaticMarkup`
 * (ModeEditor.test.tsx hit the same wall). That snapshot is the object the
 * store creator returned, so seeding it in place is what a server render
 * reads. Both readings are seeded together because a row mixes them: the value
 * arrives through a selector, `isUpdating` through a live store function. */
const SERVER_SNAPSHOTS = [
  useSettingsStore.getInitialState(),
  useModelStore.getInitialState(),
] as const;
const FRESH_INSTALL = SERVER_SNAPSHOTS.map((snapshot) => ({ ...snapshot }));

afterAll(() => {
  SERVER_SNAPSHOTS.forEach((snapshot, index) =>
    Object.assign(snapshot, FRESH_INSTALL[index]),
  );
});

/* A value away from the 0.18 default, so the reset affordance is on the row:
 * both controls write the same unread field, so a live reset is the same
 * defect as a live drag. */
const paint = (currentModel: string): string => {
  const settings = { settings: { word_correction_threshold: 0.4 } };
  const models = { models: CATALOG, currentModel };
  useSettingsStore.setState(settings);
  useModelStore.setState(models);
  Object.assign(SERVER_SNAPSHOTS[0], settings);
  Object.assign(SERVER_SNAPSHOTS[1], models);

  return renderToStaticMarkup(
    <I18nextProvider i18n={shipped}>
      <TooltipProvider>
        <WordCorrectionThreshold />
      </TooltipProvider>
    </I18nextProvider>,
  );
};

/* Radix puts the state on the slider root, which is also the element whose
 * `aria-disabled` assistive tech reads. */
const sliderIsDisabled = (markup: string): boolean =>
  /aria-disabled="true"[^>]*data-slot="slider"/.test(markup);

const resetIsDisabled = (markup: string): boolean =>
  /aria-label="Reset[^"]*"[^>]*disabled/.test(markup);

/* The row's own name reaches the markup twice on a hinted row: once naming the
 * slider thumb, once naming the affordance that carries the sentence. */
const namedControls = (markup: string): number =>
  markup.split('aria-label="Word correction threshold"').length - 1;

describe("the word correction threshold row", () => {
  test("hands a Parakeet model the live slider, its value, and no excuse", () => {
    const markup = paint("parakeet-tdt-0.6b-v3");

    expect(markup).not.toContain("__MISSING__");
    expect(markup).toContain("0.40");
    expect(sliderIsDisabled(markup)).toBe(false);
    expect(resetIsDisabled(markup)).toBe(false);
    expect(namedControls(markup)).toBe(1);
  });

  for (const [id, name] of [
    ["handy-computer/whisper-medium-gguf", "Whisper Medium"],
    ["handy-computer/Breeze-ASR-25-gguf", "Breeze ASR"],
  ]) {
    test(`refuses the drag on ${name} and says the threshold is unread`, () => {
      const markup = paint(id);

      expect(markup).not.toContain("__MISSING__");
      expect(markup).toContain(
        shipped.t("settings.debug.wordCorrectionThreshold.notApplied"),
      );
      // A number here is the claim that the drag lands somewhere.
      expect(markup).not.toContain("0.40");
      expect(sliderIsDisabled(markup)).toBe(true);
      expect(resetIsDisabled(markup)).toBe(true);
      expect(namedControls(markup)).toBe(2);
    });
  }

  test("names the models the threshold does reach", () => {
    /* Radix keeps tooltip content out of a closed tooltip, so the sentence
     * itself is asserted on the catalogue: a reader who opens the affordance
     * above has to be told where the threshold went. */
    const hint = shipped.t(
      "settings.debug.wordCorrectionThreshold.promptedHint",
    );

    expect(hint).toContain("Parakeet");
    expect(hint).toContain("Moonshine");
  });
});
