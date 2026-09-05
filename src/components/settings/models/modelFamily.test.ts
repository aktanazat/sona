import { describe, expect, test } from "bun:test";
import type { ModelInfo } from "@/bindings";
import {
  familyOf,
  groupModelsByFamily,
  isLegacyModel,
  quantLabelOf,
  takesVocabularyAsPrompt,
} from "./modelFamily";

const FALLBACKS = { custom: "Added by you", other: "Other models" };

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

describe("familyOf", () => {
  test("recovers the family from real catalog ids", () => {
    const cases: [string, string, string][] = [
      [
        "handy-computer/whisper-large-v3-gguf/whisper-large-v3-Q5_K_M.gguf",
        "Whisper Large v3",
        "whisper",
      ],
      [
        "handy-computer/parakeet-unified-en-0.6b-gguf/parakeet-unified-en-0.6b-Q8_0.gguf",
        "Parakeet Unified EN 0.6B",
        "parakeet",
      ],
      [
        "handy-computer/moonshine-base-uk-gguf/moonshine-base-uk-Q8_0.gguf",
        "Moonshine Base (Ukrainian)",
        "moonshine",
      ],
      [
        "handy-computer/gigaam-v3-rnnt-gguf/gigaam-v3-rnnt-Q8_0.gguf",
        "GigaAM v3 RNN-T",
        "gigaam",
      ],
      [
        "handy-computer/Voxtral-Mini-3B-2507-gguf/Voxtral-Mini-3B-Q4_K_M.gguf",
        "Voxtral Mini 3B",
        "voxtral",
      ],
      // Legacy ONNX ids carry no repo slug; the name is what resolves them.
      ["gigaam-v3-e2e-ctc", "GigaAM v3", "gigaam"],
      ["sense-voice-int8", "SenseVoice", "sensevoice"],
      ["canary-180m-flash", "Canary 180M Flash", "canary"],
    ];
    for (const [id, name, expected] of cases) {
      expect(familyOf(model({ id, name }), FALLBACKS).key).toBe(expected);
    }
  });

  test("Canary-Qwen is a Canary model, not a Qwen3 one", () => {
    // Rule order is the only thing that makes this true.
    const canaryQwen = model({
      id: "handy-computer/canary-qwen-2.5b-gguf/canary-qwen-2.5b-Q8_0.gguf",
      name: "Canary-Qwen 2.5B",
    });
    expect(familyOf(canaryQwen, FALLBACKS).key).toBe("canary");
  });

  test("a user-dropped file with no recognisable family lands in its own bucket", () => {
    const dropped = model({
      id: "custom-notes",
      name: "notes-model",
      source: "Local",
      is_custom: true,
    });
    expect(familyOf(dropped, FALLBACKS).label).toBe("Added by you");
  });

  test("an unrecognised download is not silently filed under a product family", () => {
    const unknown = model({ id: "someone/mystery-gguf", name: "Mystery" });
    expect(familyOf(unknown, FALLBACKS).label).toBe("Other models");
  });
});

/* Which models read the word-correction threshold, asked from the other side:
 * a whisper-family decoder gets the vocabulary as a decode prompt and the
 * backend then corrects exact repeats only, so the threshold governs nothing
 * there (`vocabulary_already_prompted`, managers/transcription.rs). The row
 * that dims on this answer names Parakeet and Moonshine as the models it does
 * govern, so a needle added to the wrong side turns that copy into a lie. */
describe("takesVocabularyAsPrompt", () => {
  const cases: [string, string, boolean][] = [
    [
      "handy-computer/whisper-large-v3-gguf/whisper-large-v3-Q5_K_M.gguf",
      "Whisper Large v3",
      true,
    ],
    // A Whisper fine-tune under its own product name: architecture "whisper"
    // in the catalog, so the decoder takes the prompt.
    ["handy-computer/Breeze-ASR-25-gguf", "Breeze ASR", true],
    ["parakeet-tdt-0.6b-v3", "Parakeet V3", false],
    ["handy-computer/moonshine-base-gguf", "Moonshine Base", false],
    ["canary-180m-flash", "Canary 180M Flash", false],
  ];
  for (const [id, name, prompted] of cases) {
    test(`${name} ${prompted ? "takes" : "does not take"} the vocabulary as a prompt`, () => {
      expect(takesVocabularyAsPrompt(model({ id, name }))).toBe(prompted);
    });
  }
});

describe("groupModelsByFamily", () => {
  test("keeps the order the backend ranked, both across and inside groups", () => {
    // get_available_models returns catalog editorial rank first, so grouping
    // must not reorder anything: the family holding the top-ranked model leads.
    const ranked = [
      model({ id: "a/parakeet-unified", name: "Parakeet Unified EN 0.6B" }),
      model({ id: "a/nemotron-streaming", name: "Nemotron Streaming 3.5" }),
      model({ id: "a/whisper-medium", name: "Whisper Medium" }),
      model({ id: "a/parakeet-tdt-v3", name: "Parakeet TDT 0.6B v3" }),
      model({ id: "a/whisper-tiny", name: "Whisper Tiny" }),
    ];
    const groups = groupModelsByFamily(ranked, FALLBACKS);
    expect(groups.map((group) => group.key)).toEqual([
      "parakeet",
      "nemotron",
      "whisper",
    ]);
    expect(groups[0].models.map((entry) => entry.name)).toEqual([
      "Parakeet Unified EN 0.6B",
      "Parakeet TDT 0.6B v3",
    ]);
    expect(groups[2].models.map((entry) => entry.name)).toEqual([
      "Whisper Medium",
      "Whisper Tiny",
    ]);
  });

  test("every model reaches exactly one group", () => {
    const models = [
      model({ id: "a/whisper-base", name: "Whisper Base" }),
      model({ id: "a/canary-1b", name: "Canary 1B" }),
      model({ id: "b/mystery", name: "Mystery" }),
    ];
    const groups = groupModelsByFamily(models, FALLBACKS);
    const total = groups.reduce((sum, group) => sum + group.models.length, 0);
    expect(total).toBe(models.length);
  });
});

describe("isLegacyModel", () => {
  test("only Url-sourced blobs are legacy", () => {
    const legacy = model({
      id: "small",
      source: { Url: { url: "https://blob/ggml-small.bin", sha256: null } },
    });
    const catalog = model({ id: "a/whisper-small" });
    const local = model({ id: "custom", source: "Local" });
    expect(isLegacyModel(legacy)).toBe(true);
    expect(isLegacyModel(catalog)).toBe(false);
    expect(isLegacyModel(local)).toBe(false);
  });
});

describe("quantLabelOf", () => {
  test("reads the quant off a GGUF filename and nothing else", () => {
    expect(quantLabelOf("whisper-medium-Q5_K_M.gguf")).toBe("Q5_K_M");
    expect(quantLabelOf("parakeet-unified-en-0.6b-Q8_0.gguf")).toBe("Q8_0");
    expect(quantLabelOf("moonshine-base-F16.gguf")).toBe("F16");
    expect(quantLabelOf("ggml-small.bin")).toBeNull();
    expect(quantLabelOf("parakeet-tdt-0.6b-v3-int8")).toBeNull();
  });
});
