import type {
  AppSettings,
  AutoSubmitKey,
  ClipboardHandling,
  CloudSttProvider,
  ContextPolicy,
  ModeAsrSettings,
  ModeDefinition,
  ModeDeliverySettings,
  ModeLlmSettings,
  ModeMutationError,
  ModePromptSettings,
  ModeView,
  ModelInfo,
  PasteMethod,
  PromptPreset,
  RequestedEngine,
  Tone,
  TypingTool,
  WebsiteHostMatch,
} from "@/bindings";
import {
  CLOUD_STT_PROVIDERS,
  cloudSttProviderForEngine,
  type CloudSttProviderMetadata,
} from "@/lib/cloudStt";

/** A `{ value, label }` pair for a picker. The kit's Select takes these. */
export interface ModeSelectOption {
  value: string;
  label: string;
}

/* Shape, constants and pure mappers shared by the modes page. Everything here
 * is total and side-effect free: the page owns state, the sections own markup. */

export const DEFAULT_MODE_ID = "message";

/**
 * The modes Sona ships with. They are presets: a starting point a reader
 * recognizes, not a private list the app depends on — every one of them can be
 * renamed, reordered and (except the default) deleted like any other mode.
 * `modes.rs:501-504` is where they are created, and this is the only place the
 * frontend restates that set.
 */
export const PRESET_MODE_IDS: readonly string[] = [
  DEFAULT_MODE_ID,
  "email",
  "meeting",
  "notes",
];

export const isPresetMode = (modeId: string): boolean =>
  PRESET_MODE_IDS.includes(modeId);

export const WEBSITE_HOST_MATCHES = [
  "exact",
  "suffix",
] as const satisfies readonly WebsiteHostMatch[];

export const CONTEXT_POLICIES = [
  "none",
  "target",
  "target_and_selection",
  "full",
] as const satisfies readonly ContextPolicy[];

export const PROMPT_PRESETS = [
  "minimalist_cleanup",
  "application_context",
  "email",
  "meeting",
  "notes",
  "generic",
] as const satisfies readonly PromptPreset[];

export const TONES = [
  "casual",
  "semi_casual",
  "balanced",
  "semi_formal",
  "formal",
] as const satisfies readonly Tone[];

export const REQUESTED_ENGINES = [
  "local",
  "deepgram_nova_3",
  "eleven_labs_scribe_v2",
] as const satisfies readonly RequestedEngine[];

export const PASTE_METHODS = [
  "ctrl_v",
  "direct",
  "none",
  "shift_insert",
  "ctrl_shift_v",
  "external_script",
] as const satisfies readonly PasteMethod[];

export const CLIPBOARD_HANDLING = [
  "copy_to_clipboard",
  "dont_modify",
] as const satisfies readonly ClipboardHandling[];

export const AUTO_SUBMIT_KEYS = [
  "enter",
  "ctrl_enter",
  "cmd_enter",
] as const satisfies readonly AutoSubmitKey[];

export const TYPING_TOOLS = [
  "auto",
  "wtype",
  "kwtype",
  "dotool",
  "ydotool",
  "xdotool",
] as const satisfies readonly TypingTool[];

export const DEFAULT_FALLBACK_MODEL_OPTION = "__mode_local_model__";

/* Every mutation error the backend can return needs a sentence. The
 * `satisfies` keeps this exhaustive when a new variant lands, and the values
 * are inline i18n defaults so no locale file has to be edited here. */
export const MODE_MUTATION_ERROR_DEFAULTS = {
  stale_revision:
    "Settings changed elsewhere. Review the latest mode and save again.",
  invalid_mode_id: "This mode ID is invalid.",
  empty_name: "A mode name is required.",
  cannot_delete_default: "The default mode cannot be deleted.",
  unknown_mode: "That mode no longer exists.",
  duplicate_mode_id: "A mode with this ID already exists.",
  invalid_reorder: "The mode order could not be saved.",
  invalid_app_identity: "That application could not be identified.",
  frontmost_application_unavailable:
    "No application in front could be captured.",
  invalid_website_host: "This website host is invalid.",
  website_activation_consent_required:
    "Enable Browser URLs in Privacy before adding a website rule.",
  frontmost_website_unavailable: "No browser website could be captured.",
  website_activation_secure_field:
    "Website rules cannot be captured from a secure field.",
  not_persisted: "That change was not saved to disk. Try again.",
} as const satisfies Record<ModeMutationError["kind"], string>;

export const downloadedModelOptions = (
  models: ModelInfo[],
): ModeSelectOption[] => {
  const options: ModeSelectOption[] = [];
  for (const model of models) {
    if (model.is_downloaded) {
      options.push({ value: model.id, label: model.name });
    }
  }
  return options;
};

/**
 * `t` narrowed to the two arguments this module uses. Taking the translator
 * instead of exporting a table keeps the mapping total and testable while
 * leaving i18n to the caller.
 */
export type ModeTranslate = (key: string, fallback: string) => string;

/**
 * The engine a run in this mode will use — the one fact about a mode's
 * configuration the list carries, because it is the one a reader compares
 * modes on. Model, language and delivery are in the editor; printing them in
 * the row too was the same four values in two places.
 */
export const modeEngineLabel = (
  asr: ModeAsrSettings,
  t: ModeTranslate,
): string => {
  /* Absent deserializes as local: an older mode predates the engine choice. */
  switch (asr.requested_engine ?? "local") {
    case "deepgram_nova_3":
      return t("settings.modes.summary.engine.deepgram_nova_3", "Deepgram");
    case "eleven_labs_scribe_v2":
      return t(
        "settings.modes.summary.engine.eleven_labs_scribe_v2",
        "ElevenLabs",
      );
    default:
      return t("settings.modes.summary.engine.local", "Local");
  }
};

export interface ModeEngineOption {
  value: RequestedEngine;
  label: string;
  /** No saved key for this provider, so it cannot be chosen yet. */
  disabled: boolean;
  /** Cloud providers group under one label; the local engine stands alone. */
  cloud: boolean;
}

/**
 * Every engine the editor offers, in menu order.
 *
 * A provider without a saved key stays in the list carrying the reason it
 * cannot be picked. Hiding it instead would leave a user who has not saved a
 * key with no way to learn the route exists — and the reason has to travel
 * with the option, because the menu is a portal that only exists once it is
 * open.
 */
export const modeEngineOptions = (
  isConfigured: (provider: CloudSttProvider) => boolean,
  t: (key: string, options?: Record<string, string>) => string,
): ModeEngineOption[] => [
  {
    value: "local",
    label: t("settings.modes.recognition.engine.local"),
    disabled: false,
    cloud: false,
  },
  ...CLOUD_STT_PROVIDERS.map((provider) => {
    const configured = isConfigured(provider.provider);
    return {
      value: provider.provider,
      label: configured
        ? t(provider.labelKey)
        : t("settings.modes.recognition.engine.unavailable", {
            provider: t(provider.labelKey),
          }),
      disabled: !configured,
      cloud: true,
    };
  }),
];

export const MODE_ROW_ACTIONS = [
  "activate",
  "duplicate",
  "moveUp",
  "moveDown",
  "delete",
] as const;

export type ModeRowActionId = (typeof MODE_ROW_ACTIONS)[number];

export interface ModeRowAction {
  id: ModeRowActionId;
  label: string;
  disabled: boolean;
  destructive: boolean;
}

/**
 * Every revisioned action a row offers, resolved against the row's position
 * and the mutation in flight. Rendering it is the menu's only job, which is
 * what makes "the default mode cannot be deleted" and "the top row cannot
 * move up" provable without opening a menu.
 *
 * The active mode has nothing to activate, so that entry drops out entirely
 * rather than sitting in the menu greyed.
 */
export const modeRowActions = (
  mode: ModeView,
  options: {
    index: number;
    count: number;
    isActive: boolean;
    busy: boolean;
    t: ModeTranslate;
  },
): ModeRowAction[] => {
  const { index, count, isActive, busy, t } = options;
  const isDefault = mode.id === DEFAULT_MODE_ID;
  const actions: ModeRowAction[] = [];
  if (!isActive) {
    actions.push({
      id: "activate",
      label: t("settings.modes.activate", "Activate"),
      disabled: busy,
      destructive: false,
    });
  }
  actions.push(
    {
      id: "duplicate",
      label: t("settings.modes.duplicate", "Duplicate"),
      disabled: busy,
      destructive: false,
    },
    /* The keyboard route to the same reorder the pointer drags, and the only
     * route for anyone not using a pointer. */
    {
      id: "moveUp",
      label: t("settings.modes.moveUp", "Move up"),
      disabled: busy || index === 0,
      destructive: false,
    },
    {
      id: "moveDown",
      label: t("settings.modes.moveDown", "Move down"),
      disabled: busy || index === count - 1,
      destructive: false,
    },
    {
      id: "delete",
      label: isDefault
        ? t(
            "settings.modes.defaultProtected",
            "The default mode cannot be deleted.",
          )
        : t("settings.modes.delete", "Delete"),
      disabled: busy || isDefault,
      destructive: true,
    },
  );
  return actions;
};

/**
 * The order the list would have after nudging one mode by one position.
 *
 * The list can be reordered two ways — dragging a row, or the move up/down
 * menu items — and the backend takes only a full ordered ID list. The drag
 * already produces one; this is what the keyboard path produces, so both
 * commit through the same command with the same shape. Returns the input
 * unchanged when the move would leave the list, which is what a disabled
 * menu item means.
 */
export const orderWithMove = (
  orderedIds: readonly string[],
  modeId: string,
  direction: -1 | 1,
): string[] => {
  const from = orderedIds.indexOf(modeId);
  const to = from + direction;
  if (from < 0 || to < 0 || to >= orderedIds.length) return [...orderedIds];
  const next = [...orderedIds];
  [next[from], next[to]] = [next[to], next[from]];
  return next;
};

/** The resolved answer to "which provider does this mode use". */
export interface ModeLlmDestinationView {
  providerId: string;
  modelId: string;
  /** The mode named no provider of its own and took the global one. */
  inherited: boolean;
}

/* A select item cannot carry an empty value, and `null` is what the draft
 * stores for "inherit the global provider". The sentinel lives between the
 * Select and its handler only: `null` is still what reaches `updateLlm`. The
 * same shape as `ModeEditor`'s `INHERIT_GLOBAL` for the transcription model,
 * and as `DEFAULT_FALLBACK_MODEL_OPTION` above. */
export const INHERIT_GLOBAL_PROVIDER = "__mode_inherit_global_provider__";

/**
 * Which post-processing provider a mode will actually use.
 *
 * The mirror of `ModeLlmSettings::destination` in `modes.rs`, and the only
 * copy of that rule on this side. It exists because the mode editor renders an
 * unsaved draft: the backend cannot answer for a provider the user picked two
 * keystrokes ago, and a row that names a provider the run will not use is the
 * defect this pair was built to close.
 *
 * The rule is one fallback and nothing else — no consent, credential or
 * endpoint state — precisely so the two copies cannot drift apart.
 */
export const modeLlmDestination = (
  llm: Pick<ModeLlmSettings, "provider_id" | "model_id">,
  global: AppSettings | null,
): ModeLlmDestinationView => {
  /* Absent and null are the same claim — this mode named no provider. A store
   * written before the field existed omits it entirely. */
  const override = llm.provider_id ?? null;
  if (override !== null) {
    return { providerId: override, modelId: llm.model_id, inherited: false };
  }
  const providerId = global?.post_process_provider_id ?? "";
  return {
    providerId,
    modelId: global?.post_process_models?.[providerId] ?? "",
    inherited: true,
  };
};

export const modeDefinitionFromView = (mode: ModeView): ModeDefinition => ({
  id: mode.id,
  name: mode.name,
  tone: mode.tone,
  context_policy: mode.context_policy,
  asr: {
    ...mode.asr,
    custom_words: [...mode.asr.custom_words],
    custom_filler_words: mode.asr.custom_filler_words
      ? [...mode.asr.custom_filler_words]
      : null,
    cloud_keyterms: mode.asr.cloud_keyterms ? [...mode.asr.cloud_keyterms] : [],
  },
  llm: { ...mode.llm },
  prompt: { ...mode.prompt },
  delivery: { ...mode.delivery },
});

/* A cloud run without word timestamps is not trustworthy enough to deliver,
 * so the backend rejects it. Repair the draft on the way to Save instead of
 * letting the user hit a rejection they cannot see the cause of. */
export const modeWithRequiredCloudTimestamps = (
  mode: ModeDefinition,
): ModeDefinition => {
  if (
    !cloudSttProviderForEngine(mode.asr.requested_engine) ||
    mode.asr.cloud_timestamps
  ) {
    return mode;
  }

  return {
    ...mode,
    asr: { ...mode.asr, cloud_timestamps: true },
  };
};

/* The default mode's dictation chord is the app-wide `transcribe` binding, not
 * a mode-scoped one. Every other chord is derived from the mode ID. */
export const modeBindingId = (
  modeId: string,
  kind: "transcribe" | "switch",
): string =>
  kind === "transcribe" && modeId === DEFAULT_MODE_ID
    ? "transcribe"
    : `mode/${modeId}/${kind}`;

/* CONTEXT_POLICIES is ordered least to most revealing, so index order is the
 * escalation order the privacy ceiling clamps against. */
export const hasHigherPolicy = (
  policy: ContextPolicy,
  ceiling: ContextPolicy,
): boolean =>
  CONTEXT_POLICIES.indexOf(policy) > CONTEXT_POLICIES.indexOf(ceiling);

/* Both sides of this comparison are built by `modeDefinitionFromView` and the
 * draft only ever replaces existing keys, so key order matches and a string
 * compare is enough to tell a touched draft from a saved one. A wrong answer
 * would only mislabel the header, never lose an edit. */
export const modeDraftIsDirty = (
  draft: ModeDefinition,
  saved: ModeView | undefined,
): boolean =>
  saved !== undefined &&
  JSON.stringify(draft) !== JSON.stringify(modeDefinitionFromView(saved));

export interface ModeDraftUpdaters {
  update: <K extends keyof ModeDefinition>(
    key: K,
    value: ModeDefinition[K],
  ) => void;
  updateAsr: <K extends keyof ModeAsrSettings>(
    key: K,
    value: ModeAsrSettings[K],
  ) => void;
  updateLlm: <K extends keyof ModeLlmSettings>(
    key: K,
    value: ModeLlmSettings[K],
  ) => void;
  updatePrompt: <K extends keyof ModePromptSettings>(
    key: K,
    value: ModePromptSettings[K],
  ) => void;
  updateDelivery: <K extends keyof ModeDeliverySettings>(
    key: K,
    value: ModeDeliverySettings[K],
  ) => void;
  replace: (mode: ModeDefinition) => void;
}

export const createModeDraftUpdaters = (
  mode: ModeDefinition,
  onChange: (next: ModeDefinition) => void,
): ModeDraftUpdaters => ({
  update: <K extends keyof ModeDefinition>(key: K, value: ModeDefinition[K]) =>
    onChange({ ...mode, [key]: value }),
  updateAsr: <K extends keyof ModeAsrSettings>(
    key: K,
    value: ModeAsrSettings[K],
  ) => onChange({ ...mode, asr: { ...mode.asr, [key]: value } }),
  updateLlm: <K extends keyof ModeLlmSettings>(
    key: K,
    value: ModeLlmSettings[K],
  ) => onChange({ ...mode, llm: { ...mode.llm, [key]: value } }),
  updatePrompt: <K extends keyof ModePromptSettings>(
    key: K,
    value: ModePromptSettings[K],
  ) => onChange({ ...mode, prompt: { ...mode.prompt, [key]: value } }),
  updateDelivery: <K extends keyof ModeDeliverySettings>(
    key: K,
    value: ModeDeliverySettings[K],
  ) => onChange({ ...mode, delivery: { ...mode.delivery, [key]: value } }),
  replace: onChange,
});

/* Cloud eligibility is probed once per editor mount, so the panels receive the
 * resolved answer instead of each running their own keyring query. */
export interface ModeCloudState {
  requestedEngine: RequestedEngine;
  selectedProvider: CloudSttProviderMetadata | undefined;
  /** A provider with a saved native key. Consent is tracked separately. */
  isConfigured: (provider: CloudSttProvider) => boolean;
  /** The selected cloud provider has both a key and current transfer consent. */
  controlsAvailable: boolean;
  selectEngine: (engine: RequestedEngine) => void;
}

export interface ModePanelProps {
  mode: ModeDefinition;
  updaters: ModeDraftUpdaters;
}
