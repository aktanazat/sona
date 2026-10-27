import { describe, expect, test } from "bun:test";
import type { ModelInfo } from "@/bindings";
import {
  ALL_FAMILIES,
  buildSearchIndex,
  diskUsage,
  filterModels,
  isFiltered,
  NO_FILTERS,
  visibleCatalog,
} from "./modelCatalog";

const FALLBACKS = { custom: "Added by you", other: "Other models" };

/* The English words the page passes in. Nothing else here translates. */
const WORDS = { streaming: "Streaming", translation: "Translate" };

const model = (overrides: Partial<ModelInfo> & { id: string }): ModelInfo => ({
  name: overrides.id,
  description: "",
  filename: "model.gguf",
  source: { HuggingFace: { repo_id: "handy-computer/x", revision: "main" } },
  size_mb: 100,
  is_downloaded: false,
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
  ...overrides,
});

const CATALOG: ModelInfo[] = [
  model({
    id: "a/parakeet-unified",
    name: "Parakeet Unified EN 0.6B",
    description: "Fast, accurate live English transcription",
    supports_streaming: true,
    is_downloaded: true,
    size_mb: 697,
  }),
  model({
    id: "a/canary-1b-v2",
    name: "Canary 1B v2",
    description: "Accurate multilingual",
    supported_languages: ["en", "de", "ru"],
    supports_translation: true,
    size_mb: 1024,
  }),
  model({
    id: "a/whisper-medium",
    name: "Whisper Medium",
    description: "Good accuracy, medium speed",
    supported_languages: ["en", "ru", "ja"],
    is_downloaded: true,
    size_mb: 1536,
  }),
  model({
    id: "a/moonshine-tiny",
    name: "Moonshine Tiny",
    description: "Ultra-fast, English only",
    supports_streaming: true,
  }),
];

const filter = (
  overrides: Partial<typeof NO_FILTERS>,
  models: ModelInfo[] = CATALOG,
): string[] =>
  filterModels(
    models,
    { ...NO_FILTERS, ...overrides },
    buildSearchIndex(models, WORDS),
    FALLBACKS,
  ).map((entry) => entry.name);

describe("visibleCatalog", () => {
  test("hides a legacy download that is not on disk and keeps one that is", () => {
    const legacy = (id: string, onDisk: boolean) =>
      model({
        id,
        source: { Url: { url: `https://blob/${id}.bin`, sha256: null } },
        is_downloaded: onDisk,
      });
    const visible = visibleCatalog([
      legacy("small", false),
      legacy("turbo", true),
      model({ id: "a/whisper-tiny" }),
    ]);
    expect(visible.map((entry) => entry.id)).toEqual([
      "turbo",
      "a/whisper-tiny",
    ]);
  });
});

describe("filterModels", () => {
  test("no filters means the whole catalog, in the order it arrived", () => {
    expect(filter({})).toEqual([
      "Parakeet Unified EN 0.6B",
      "Canary 1B v2",
      "Whisper Medium",
      "Moonshine Tiny",
    ]);
  });

  test("the text filter matches a name", () => {
    expect(filter({ query: "moonshine" })).toEqual(["Moonshine Tiny"]);
  });

  test("the text filter matches a description", () => {
    expect(filter({ query: "medium speed" })).toEqual(["Whisper Medium"]);
  });

  test("the text filter matches a language name, which is how language filtering works now", () => {
    expect(filter({ query: "Russian" })).toEqual([
      "Canary 1B v2",
      "Whisper Medium",
    ]);
    expect(filter({ query: "japanese" })).toEqual(["Whisper Medium"]);
  });

  test("the text filter matches a capability word", () => {
    expect(filter({ query: "streaming" })).toEqual([
      "Parakeet Unified EN 0.6B",
      "Moonshine Tiny",
    ]);
  });

  test("the text filter ignores case and surrounding space", () => {
    expect(filter({ query: "  WHISPER  " })).toEqual(["Whisper Medium"]);
  });

  test("the family filter narrows to one family", () => {
    expect(filter({ family: "whisper" })).toEqual(["Whisper Medium"]);
    expect(filter({ family: "canary" })).toEqual(["Canary 1B v2"]);
  });

  test("the on-disk filter keeps only downloaded models", () => {
    expect(filter({ downloadedOnly: true })).toEqual([
      "Parakeet Unified EN 0.6B",
      "Whisper Medium",
    ]);
  });

  test("capability filters keep only models that advertise them", () => {
    expect(filter({ streamingOnly: true })).toEqual([
      "Parakeet Unified EN 0.6B",
      "Moonshine Tiny",
    ]);
    expect(filter({ translationOnly: true })).toEqual(["Canary 1B v2"]);
  });

  test("filters compose, and an impossible combination yields nothing", () => {
    expect(filter({ downloadedOnly: true, streamingOnly: true })).toEqual([
      "Parakeet Unified EN 0.6B",
    ]);
    // Canary translates but is not on disk in this fixture.
    expect(filter({ downloadedOnly: true, translationOnly: true })).toEqual([]);
  });

  test("a family filter and a text filter must both hold", () => {
    expect(filter({ family: "whisper", query: "russian" })).toEqual([
      "Whisper Medium",
    ]);
    expect(filter({ family: "moonshine", query: "russian" })).toEqual([]);
  });
});

describe("isFiltered", () => {
  test("reports whether anything is narrowing the list", () => {
    expect(isFiltered(NO_FILTERS)).toBe(false);
    expect(isFiltered({ ...NO_FILTERS, query: "   " })).toBe(false);
    expect(isFiltered({ ...NO_FILTERS, query: "w" })).toBe(true);
    expect(isFiltered({ ...NO_FILTERS, family: "whisper" })).toBe(true);
    expect(isFiltered({ ...NO_FILTERS, family: ALL_FAMILIES })).toBe(false);
    expect(isFiltered({ ...NO_FILTERS, downloadedOnly: true })).toBe(true);
    expect(isFiltered({ ...NO_FILTERS, streamingOnly: true })).toBe(true);
    expect(isFiltered({ ...NO_FILTERS, translationOnly: true })).toBe(true);
  });
});

describe("diskUsage", () => {
  test("sums only what is on disk", () => {
    const usage = diskUsage(CATALOG);
    expect(usage.count).toBe(2);
    expect(usage.sizeMb).toBe(697 + 1536);
  });

  test("an empty catalog uses no space", () => {
    const usage = diskUsage([]);
    expect(usage.count).toBe(0);
    expect(usage.sizeMb).toBe(0);
  });
});

