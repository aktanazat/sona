import type { ModelInfo } from "@/bindings";

/* Family grouping for the model catalog.
 *
 * `src-tauri/src/catalog/catalog.json` carries a `family` field for every
 * catalog model, but `ModelInfo` — the shape the webview receives — does not
 * expose it, so the family is recovered here from the model id (which embeds
 * the Hugging Face repo slug) and the display name. Legacy `.bin` entries,
 * models discovered in the shared Hugging Face cache and user-dropped files
 * never had a catalog family at all, so the same rules are what group them.
 *
 * Rule order is load-bearing: "Canary-Qwen 2.5B" is a Canary model, so
 * `canary` is tested before `qwen3`. */

export interface ModelFamily {
  /** Stable key for the filter dropdown, grouping and React keys. */
  key: string;
  /** Display label. Product names, so they are not translated. */
  label: string;
}

interface FamilyRule extends ModelFamily {
  needles: readonly string[];
}

const FAMILY_RULES: readonly FamilyRule[] = [
  { key: "moonshine", label: "Moonshine", needles: ["moonshine"] },
  { key: "parakeet", label: "Parakeet", needles: ["parakeet"] },
  { key: "nemotron", label: "Nemotron", needles: ["nemotron"] },
  { key: "canary", label: "Canary", needles: ["canary"] },
  { key: "cohere", label: "Cohere", needles: ["cohere"] },
  { key: "voxtral", label: "Voxtral", needles: ["voxtral"] },
  {
    key: "qwen3-asr",
    label: "Qwen3-ASR",
    needles: ["qwen3-asr", "qwen3_asr", "qwen3"],
  },
  { key: "fun-asr", label: "Fun-ASR", needles: ["fun-asr", "funasr"] },
  { key: "gigaam", label: "GigaAM", needles: ["gigaam", "giga-am"] },
  { key: "granite", label: "Granite Speech", needles: ["granite"] },
  {
    key: "sensevoice",
    label: "SenseVoice",
    needles: ["sensevoice", "sense-voice"],
  },
  { key: "medasr", label: "MedASR", needles: ["medasr", "med-asr"] },
  { key: "moss", label: "MOSS", needles: ["moss"] },
  { key: "breeze", label: "Breeze ASR", needles: ["breeze"] },
  { key: "whisper", label: "Whisper", needles: ["whisper"] },
];

export const CUSTOM_FAMILY_KEY = "custom";
export const OTHER_FAMILY_KEY = "other";

/** Labels for the two buckets that are not product families. */
export interface FallbackFamilyLabels {
  /** Files the user dropped into the models folder themselves. */
  custom: string;
  /** Recognised by the engine, but matching no known family. */
  other: string;
}

const familyRuleFor = (model: ModelInfo): FamilyRule | undefined => {
  const haystack = `${model.id} ${model.name}`.toLowerCase();
  return FAMILY_RULES.find((rule) =>
    rule.needles.some((needle) => haystack.includes(needle)),
  );
};

export const familyOf = (
  model: ModelInfo,
  fallbacks: FallbackFamilyLabels,
): ModelFamily => {
  const rule = familyRuleFor(model);
  if (rule) return { key: rule.key, label: rule.label };
  return model.is_custom
    ? { key: CUSTOM_FAMILY_KEY, label: fallbacks.custom }
    : { key: OTHER_FAMILY_KEY, label: fallbacks.other };
};

/**
 * Whether this model's decoder is handed the user's vocabulary as a decode
 * prompt, which is what makes fuzzy correction afterwards a second guess at
 * words the decoder was already told about. `managers/transcription.rs` gates
 * that on `model.arch() == "whisper"`.
 *
 * Breeze ASR answers true while wearing its own product family: Breeze-ASR-25
 * is a Whisper fine-tune, `architecture: "whisper"` in the catalog, and the
 * models page still lists it under its own name.
 *
 * Read off the id and the name for the same reason `familyOf` is — `ModelInfo`
 * carries no architecture. A user-dropped GGUF whose filename says nothing
 * about its architecture therefore reads as false, which is the conservative
 * answer: a control stays live rather than dimming on a guess.
 */
export const takesVocabularyAsPrompt = (model: ModelInfo): boolean => {
  const key = familyRuleFor(model)?.key;
  return key === "whisper" || key === "breeze";
};

export interface ModelFamilyGroup extends ModelFamily {
  models: ModelInfo[];
}

/**
 * Group `models` by family, preserving the order the backend returned.
 *
 * `get_available_models` already sorts by the catalog's editorial rank
 * (recommended first), so ordering groups by first appearance and rows by
 * arrival keeps that ranking intact without the frontend inventing a score of
 * its own: the family holding the highest-ranked model leads the page.
 */
export const groupModelsByFamily = (
  models: readonly ModelInfo[],
  fallbacks: FallbackFamilyLabels,
): ModelFamilyGroup[] => {
  const groups = new Map<string, ModelFamilyGroup>();
  for (const model of models) {
    const family = familyOf(model, fallbacks);
    const group = groups.get(family.key);
    if (group) {
      group.models.push(model);
    } else {
      groups.set(family.key, { ...family, models: [model] });
    }
  }
  return [...groups.values()];
};

/** Extract a GGUF quantization label from a filename, if present ("Q8_0"). */
export const quantLabelOf = (filename: string): string | null => {
  const match = filename.match(
    /[._-](IQ\d+_\w+|Q\d+(?:_\w+)?|F16|BF16|F32)\.gguf$/i,
  );
  return match ? match[1].toUpperCase() : null;
};

/**
 * Legacy models are the blob (`Url`-sourced) `.bin`/ONNX downloads, superseded
 * by the catalog GGUFs. They stay runnable while on disk, but the download is
 * no longer advertised.
 */
export const isLegacyModel = (model: ModelInfo): boolean => {
  if (model.source === "Local") return false;
  return "Url" in model.source;
};
