import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";
import { listen } from "@tauri-apps/api/event";
import {
  commands,
  type AppSettings as Settings,
  type AudioDevice,
  type PostProcessModelCatalog,
  type PostProcessModelOption,
  type Result,
} from "@/bindings";
export type PostProcessModelCatalogState = {
  catalog: PostProcessModelCatalog;
  /** The last successful list for this exact provider/configuration. */
  cachedModels: PostProcessModelOption[];
};

/** A remote list is valid only for the provider and endpoint that produced it. */
export const postProcessModelCatalogScope = (
  providerId: string,
  baseUrl: string,
): string => `${providerId}\u0000${baseUrl.trim()}`;

const catalogScopeForSettings = (
  settings: Settings | null,
  providerId: string,
): string => {
  const baseUrl =
    settings?.post_process_providers?.find(
      (provider) => provider.id === providerId,
    )?.base_url ?? "";
  return postProcessModelCatalogScope(providerId, baseUrl);
};

/* An invalidation must also make an in-flight result ineligible to write back.
 * The counters are transport bookkeeping, not rendered state. */
const catalogProviderRevisions = new Map<string, number>();
const catalogRequestRevisions = new Map<string, number>();

const fallbackCatalog = (
  providerId: string,
  discovery: PostProcessModelCatalog["discovery"],
  allowsManualModelId: boolean,
): PostProcessModelCatalog => ({
  provider_id: providerId,
  models: [],
  discovery,
  allows_manual_model_id: allowsManualModelId,
});

export interface SettingsStore {
  settings: Settings | null;
  defaultSettings: Settings | null;
  isLoading: boolean;
  isUpdating: Record<string, boolean>;
  audioDevices: AudioDevice[];
  outputDevices: AudioDevice[];
  customSounds: { start: boolean; stop: boolean };
  postProcessModelCatalogs: Record<string, PostProcessModelCatalogState>;

  // Actions
  initialize: () => Promise<void>;
  loadDefaultSettings: () => Promise<void>;
  updateSetting: <K extends keyof Settings>(
    key: K,
    value: SettingValue<K>,
  ) => Promise<void>;
  resetSetting: (key: keyof Settings) => Promise<void>;
  refreshSettings: () => Promise<void>;
  refreshAudioDevices: () => Promise<void>;
  refreshOutputDevices: () => Promise<void>;
  updateBinding: (id: string, binding: string) => Promise<void>;
  resetBinding: (id: string) => Promise<void>;
  getSetting: <K extends keyof Settings>(key: K) => Settings[K] | undefined;
  getDefaultSetting: <K extends keyof Settings>(
    key: K,
  ) => Settings[K] | undefined;
  isUpdatingKey: (key: string) => boolean;
  playTestSound: (soundType: "start" | "stop") => Promise<void>;
  checkCustomSounds: () => Promise<void>;
  setPostProcessProvider: (providerId: string) => Promise<void>;
  updatePostProcessBaseUrl: (
    providerId: string,
    baseUrl: string,
  ) => Promise<void>;
  replacePostProcessSecret: (
    providerId: string,
    secret: string,
  ) => Promise<boolean>;
  removePostProcessSecret: (providerId: string) => Promise<boolean>;
  refreshPostProcessSecretState: (providerId: string) => Promise<void>;
  updatePostProcessModel: (providerId: string, model: string) => Promise<void>;
  discoverPostProcessModelCatalog: (
    providerId: string,
  ) => Promise<PostProcessModelCatalog>;
  invalidatePostProcessModelCatalog: (providerId: string) => void;

  // Internal state setters
  setSettings: (settings: Settings | null) => void;
  setDefaultSettings: (defaultSettings: Settings | null) => void;
  setLoading: (loading: boolean) => void;
  setUpdating: (key: string, updating: boolean) => void;
  setAudioDevices: (devices: AudioDevice[]) => void;
  setOutputDevices: (devices: AudioDevice[]) => void;
  setCustomSounds: (sounds: { start: boolean; stop: boolean }) => void;
}

// Note: Default settings are now fetched from Rust via commands.getDefaultSettings()
// This ensures platform-specific defaults (like overlay_position, shortcuts, paste_method) work correctly

const DEFAULT_AUDIO_DEVICE: AudioDevice = {
  index: "default",
  name: "Default",
  is_default: true,
};

/* One setting's own value type with the "not present in the store" case
 * removed. Every `AppSettings` field is optional on the wire because Rust
 * serializes it from an `Option`, but a write always carries a value. */
export type SettingValue<K extends keyof Settings> = Exclude<
  Settings[K],
  undefined
>;

/* The updater registered for a key receives that key's own value type, so
 * every command call below is checked against the setting it writes. Each one
 * is a settings-write command, and those all answer with the same `Result`. */
type SettingUpdaters = {
  [K in keyof Settings]?: (
    value: SettingValue<K>,
  ) => Promise<Result<null, string>>;
};

/* "Default" is the label the device pickers show for "whatever the OS
 * picks"; the backend spells that `default`, and a cleared setting means the
 * same thing. */
const deviceName = (value: string | null): string =>
  value === null || value === "Default" ? "default" : value;

const settingUpdaters: SettingUpdaters = {
  always_on_microphone: (value) => commands.updateMicrophoneMode(value),
  audio_feedback: (value) => commands.changeAudioFeedbackSetting(value),
  audio_feedback_volume: (value) =>
    commands.changeAudioFeedbackVolumeSetting(value),
  sound_theme: (value) => commands.changeSoundThemeSetting(value),
  start_hidden: (value) => commands.changeStartHiddenSetting(value),
  autostart_enabled: (value) => commands.changeAutostartSetting(value),
  show_whats_new_on_update: (value) =>
    commands.changeShowWhatsNewOnUpdateSetting(value),
  whats_new_last_seen_version: (value) =>
    commands.changeWhatsNewLastSeenVersionSetting(value),
  push_to_talk: (value) => commands.changePttSetting(value),
  command_mode_enabled: (value) =>
    commands.changeCommandModeEnabledSetting(value),
  selected_microphone: (value) =>
    commands.setSelectedMicrophone(deviceName(value)),
  selected_channel: (value) => commands.setSelectedChannel(value),
  clamshell_microphone: (value) =>
    commands.setClamshellMicrophone(deviceName(value)),
  selected_output_device: (value) =>
    commands.setSelectedOutputDevice(deviceName(value)),
  recording_retention_period: (value) =>
    commands.updateRecordingRetentionPeriod(value),
  translate_to_english: (value) =>
    commands.changeTranslateToEnglishSetting(value),
  selected_language: (value) => commands.changeSelectedLanguageSetting(value),
  english_spelling: (value) => commands.changeEnglishSpellingSetting(value),
  overlay_position: (value) => commands.changeOverlayPositionSetting(value),
  debug_mode: (value) => commands.changeDebugModeSetting(value),
  word_correction_threshold: (value) =>
    commands.changeWordCorrectionThresholdSetting(value),
  external_script_path: (value) =>
    commands.changeExternalScriptPathSetting(value),
  history_limit: (value) => commands.updateHistoryLimit(value),
  post_process_enabled: (value) =>
    commands.changePostProcessEnabledSetting(value),
  /* The backend only ever *selects* a prompt that exists, so there is no
   * command for clearing the selection. Reject instead of sending a null the
   * IPC layer would refuse to deserialize. */
  post_process_selected_prompt_id: (value) =>
    value === null
      ? Promise.reject(new Error("No post-process prompt id to select"))
      : commands.setPostProcessSelectedPrompt(value),
  mute_while_recording: (value) =>
    commands.changeMuteWhileRecordingSetting(value),
  append_trailing_space: (value) =>
    commands.changeAppendTrailingSpaceSetting(value),
  log_level: (value) => commands.setLogLevel(value),
  app_language: (value) => commands.changeAppLanguageSetting(value),
  theme: (value) => commands.changeThemeSetting(value),
  /* The Rust side resolves intent against whether native vibrancy actually
   * applied and writes the resulting `data-material` into every webview
   * itself, so there is nothing to do here with the return value. */
  appearance_material: (value) =>
    commands.changeAppearanceMaterialSetting(value),
  experimental_enabled: (value) =>
    commands.changeExperimentalEnabledSetting(value),
  lazy_stream_close: (value) => commands.changeLazyStreamCloseSetting(value),
  overlay_style: (value) => commands.changeOverlayStyleSetting(value),
  vad_enabled: (value) => commands.changeVadEnabledSetting(value),
  filler_word_removal_enabled: (value) =>
    commands.changeFillerWordRemovalEnabledSetting(value),
  show_tray_icon: (value) => commands.changeShowTrayIconSetting(value),
  transcribe_accelerator: (value) =>
    commands.changeTranscribeAcceleratorSetting(value),
  ort_accelerator: (value) => commands.changeOrtAcceleratorSetting(value),
  transcribe_gpu_device: (value) => commands.changeTranscribeGpuDevice(value),
  extra_recording_buffer_ms: (value) =>
    commands.changeExtraRecordingBufferSetting(value),
  external_query_enabled: (value) =>
    commands.changeExternalQueryEnabledSetting(value),
  external_mutations_enabled: (value) =>
    commands.changeExternalMutationsEnabledSetting(value),
  meeting_remote_intelligence_enabled: (value) =>
    commands.changeMeetingRemoteIntelligenceEnabledSetting(value),
  meeting_local_engine: (value) =>
    commands.changeMeetingLocalEngineSetting(value),
  meeting_digest_enabled: (value) =>
    commands.changeMeetingDigestEnabledSetting(value),
  meeting_digest_minute_of_day: (value) =>
    commands.changeMeetingDigestMinuteOfDaySetting(value),
  model_unload_timeout: (value) => commands.setModelUnloadTimeout(value),
};

/* The in-flight (then settled) first load. See `initialize` for why the latch
 * lives beside the store rather than inside its state. */
let initialization: Promise<void> | null = null;

export const useSettingsStore = create<SettingsStore>()(
  subscribeWithSelector((set, get) => ({
    settings: null,
    defaultSettings: null,
    isLoading: true,
    isUpdating: {},
    audioDevices: [],
    outputDevices: [],
    customSounds: { start: false, stop: false },
    postProcessModelCatalogs: {},

    // Internal setters
    setSettings: (settings) => set({ settings }),
    setDefaultSettings: (defaultSettings) => set({ defaultSettings }),
    setLoading: (isLoading) => set({ isLoading }),
    setUpdating: (key, updating) =>
      set((state) => ({
        isUpdating: { ...state.isUpdating, [key]: updating },
      })),
    setAudioDevices: (audioDevices) => set({ audioDevices }),
    setOutputDevices: (outputDevices) => set({ outputDevices }),
    setCustomSounds: (customSounds) => set({ customSounds }),

    // Getters
    getSetting: (key) => get().settings?.[key],
    getDefaultSetting: (key) => get().defaultSettings?.[key],
    isUpdatingKey: (key) => get().isUpdating[key] || false,

    // Load settings from store
    refreshSettings: async () => {
      try {
        const result = await commands.getAppSettings();
        if (result.status === "ok") {
          const settings = result.data;
          const normalizedSettings: Settings = {
            ...settings,
            always_on_microphone: settings.always_on_microphone ?? false,
            selected_microphone: settings.selected_microphone ?? "Default",
            clamshell_microphone: settings.clamshell_microphone ?? "Default",
            selected_output_device:
              settings.selected_output_device ?? "Default",
          };
          set({ settings: normalizedSettings, isLoading: false });
        } else {
          console.error("Failed to load settings:", result.error);
          set({ isLoading: false });
        }
      } catch (error) {
        console.error("Failed to load settings:", error);
        set({ isLoading: false });
      }
    },

    // Load audio devices
    refreshAudioDevices: async () => {
      try {
        const result = await commands.getAvailableMicrophones();
        if (result.status === "ok") {
          const devicesWithDefault = [
            DEFAULT_AUDIO_DEVICE,
            ...result.data.filter(
              (d) => d.name !== "Default" && d.name !== "default",
            ),
          ];
          set({ audioDevices: devicesWithDefault });
        } else {
          set({ audioDevices: [DEFAULT_AUDIO_DEVICE] });
        }
      } catch (error) {
        console.error("Failed to load audio devices:", error);
        set({ audioDevices: [DEFAULT_AUDIO_DEVICE] });
      }
    },

    // Load output devices
    refreshOutputDevices: async () => {
      try {
        const result = await commands.getAvailableOutputDevices();
        if (result.status === "ok") {
          const devicesWithDefault = [
            DEFAULT_AUDIO_DEVICE,
            ...result.data.filter(
              (d) => d.name !== "Default" && d.name !== "default",
            ),
          ];
          set({ outputDevices: devicesWithDefault });
        } else {
          set({ outputDevices: [DEFAULT_AUDIO_DEVICE] });
        }
      } catch (error) {
        console.error("Failed to load output devices:", error);
        set({ outputDevices: [DEFAULT_AUDIO_DEVICE] });
      }
    },

    // Play a test sound
    playTestSound: async (soundType: "start" | "stop") => {
      try {
        await commands.playTestSound(soundType);
      } catch (error) {
        console.error(`Failed to play test sound (${soundType}):`, error);
      }
    },

    checkCustomSounds: async () => {
      try {
        const sounds = await commands.checkCustomSounds();
        get().setCustomSounds(sounds);
      } catch (error) {
        console.error("Failed to check custom sounds:", error);
      }
    },

    // Update a specific setting
    updateSetting: async <K extends keyof Settings>(
      key: K,
      value: SettingValue<K>,
    ) => {
      const { settings, setUpdating } = get();
      const updateKey = String(key);
      const originalValue = settings?.[key];

      setUpdating(updateKey, true);

      try {
        set((state) => ({
          settings: state.settings ? { ...state.settings, [key]: value } : null,
        }));

        const updater = settingUpdaters[key];
        if (updater === undefined) {
          /* Nothing sends this key, so the optimistic value above is the whole
           * of what happened and the row would be showing a write that never
           * left the app. Fail it the way a refusal fails. */
          throw new Error(`no settings command writes ${String(key)}`);
        }
        /* A refused write answers, it does not throw: the backend's `Err`
         * arrives as a resolved `{ status: "error" }`. Reaching the rollback
         * below is what keeps the row from claiming a write the backend never
         * took - on the consent rows, a grant a reader believes withdrawn
         * while it is still live. */
        const result = await updater(value);
        if (result.status === "error") throw new Error(result.error);
      } catch (error) {
        console.error(`Failed to update setting ${String(key)}:`, error);
        set((state) => ({
          settings: state.settings
            ? { ...state.settings, [key]: originalValue }
            : null,
        }));
        // A failed disk save can still change the backend's in-memory settings.
        try {
          const result = await commands.getAppSettings();
          if (result.status === "error") throw new Error(result.error);
          set((state) => ({
            settings: state.settings
              ? { ...state.settings, [key]: result.data[key] }
              : null,
          }));
        } catch (readError) {
          console.error(`Failed to reload setting ${String(key)}:`, readError);
        }
      } finally {
        setUpdating(updateKey, false);
      }
    },

    // Reset a setting to its default value
    resetSetting: async (key) => {
      const { defaultSettings } = get();
      if (defaultSettings) {
        const defaultValue = defaultSettings[key];
        if (defaultValue !== undefined) {
          await get().updateSetting(key, defaultValue);
        }
      }
    },

    // Update a specific binding
    updateBinding: async (id, binding) => {
      const { settings, setUpdating } = get();
      const updateKey = `binding_${id}`;
      const originalBinding = settings?.bindings?.[id]?.current_binding;

      setUpdating(updateKey, true);

      try {
        // Optimistic update
        set((state) => {
          const currentSettings = state.settings;
          const bindings = currentSettings?.bindings;
          const currentBinding = bindings?.[id];
          if (!currentSettings || !bindings || !currentBinding) return {};
          return {
            settings: {
              ...currentSettings,
              bindings: {
                ...bindings,
                [id]: { ...currentBinding, current_binding: binding },
              },
            },
          };
        });

        const result = await commands.changeBinding(id, binding);

        // Check if the command executed successfully
        if (result.status === "error") {
          throw new Error(result.error);
        }

        // Check if the binding change was successful
        if (!result.data.success) {
          throw new Error(result.data.error || "Failed to update binding");
        }
      } catch (error) {
        console.error(`Failed to update binding ${id}:`, error);

        // Roll back only the binding record that the optimistic update changed.
        if (originalBinding) {
          set((state) => {
            const currentSettings = state.settings;
            const bindings = currentSettings?.bindings;
            const currentBinding = bindings?.[id];
            if (!currentSettings || !bindings || !currentBinding) return {};
            return {
              settings: {
                ...currentSettings,
                bindings: {
                  ...bindings,
                  [id]: { ...currentBinding, current_binding: originalBinding },
                },
              },
            };
          });
        }

        // Re-throw to let the caller know it failed
        throw error;
      } finally {
        setUpdating(updateKey, false);
      }
    },

    // Reset a specific binding
    resetBinding: async (id) => {
      const { setUpdating, refreshSettings } = get();
      const updateKey = `binding_${id}`;

      setUpdating(updateKey, true);

      try {
        await commands.resetBinding(id);
        await refreshSettings();
      } catch (error) {
        console.error(`Failed to reset binding ${id}:`, error);
      } finally {
        setUpdating(updateKey, false);
      }
    },

    setPostProcessProvider: async (providerId) => {
      const { settings, setUpdating, refreshSettings } = get();
      const updateKey = "post_process_provider_id";
      const previousId = settings?.post_process_provider_id ?? null;

      setUpdating(updateKey, true);

      if (settings) {
        set((state) => ({
          settings: state.settings
            ? { ...state.settings, post_process_provider_id: providerId }
            : null,
        }));
      }

      try {
        const result = await commands.setPostProcessProvider(providerId);
        if (result.status === "error") throw new Error(result.error);
        await refreshSettings();
      } catch (error) {
        console.error("Failed to set post-process provider:", error);
        if (previousId !== null) {
          set((state) => ({
            settings: state.settings
              ? { ...state.settings, post_process_provider_id: previousId }
              : null,
          }));
        }
      } finally {
        setUpdating(updateKey, false);
      }
    },

    updatePostProcessBaseUrl: async (providerId, baseUrl) => {
      const {
        setUpdating,
        refreshSettings,
        invalidatePostProcessModelCatalog,
      } = get();
      const updateKey = `post_process_base_url:${providerId}`;

      setUpdating(updateKey, true);
      try {
        const urlResult = await commands.changePostProcessBaseUrlSetting(
          providerId,
          baseUrl,
        );
        if (urlResult.status === "error") {
          console.error("Failed to persist base URL:", urlResult.error);
          return;
        }

        /* A changed endpoint is a different compatibility boundary even if
         * clearing its saved model fails afterwards. */
        invalidatePostProcessModelCatalog(providerId);

        const modelResult = await commands.changePostProcessModelSetting(
          providerId,
          "",
        );
        if (modelResult.status === "error") {
          console.error("Failed to reset model setting:", modelResult.error);
          return;
        }

        await refreshSettings();
      } catch (error) {
        console.error("Failed to update post-process base URL:", error);
      } finally {
        setUpdating(updateKey, false);
      }
    },

    replacePostProcessSecret: async (providerId, secret) => {
      const updateKey = `post_process_secret:${providerId}`;
      const { setUpdating, invalidatePostProcessModelCatalog } = get();
      setUpdating(updateKey, true);

      try {
        const result = await commands.setProviderSecret(
          "llm",
          providerId,
          secret,
        );
        if (result.status === "error") {
          console.error("Failed to store provider credential:", result.error);
          return false;
        }
        set((state) => ({
          settings: state.settings
            ? {
                ...state.settings,
                post_process_secret_states: {
                  ...state.settings.post_process_secret_states,
                  [providerId]: result.data,
                },
              }
            : null,
        }));
        invalidatePostProcessModelCatalog(providerId);
        return true;
      } catch (error) {
        console.error("Failed to store provider credential:", error);
        return false;
      } finally {
        setUpdating(updateKey, false);
      }
    },

    removePostProcessSecret: async (providerId) => {
      const updateKey = `post_process_secret:${providerId}`;
      const { setUpdating, invalidatePostProcessModelCatalog } = get();
      setUpdating(updateKey, true);

      try {
        const result = await commands.deleteProviderSecret("llm", providerId);
        if (result.status === "error") {
          console.error("Failed to remove provider credential:", result.error);
          return false;
        }
        set((state) => ({
          settings: state.settings
            ? {
                ...state.settings,
                post_process_secret_states: {
                  ...state.settings.post_process_secret_states,
                  [providerId]: result.data,
                },
              }
            : null,
        }));
        invalidatePostProcessModelCatalog(providerId);
        return true;
      } catch (error) {
        console.error("Failed to remove provider credential:", error);
        return false;
      } finally {
        setUpdating(updateKey, false);
      }
    },

    refreshPostProcessSecretState: async (providerId) => {
      const updateKey = `post_process_secret:${providerId}`;
      const { setUpdating } = get();
      setUpdating(updateKey, true);

      try {
        const result = await commands.getProviderSecretState("llm", providerId);
        if (result.status === "error") {
          console.error(
            "Failed to read provider credential state:",
            result.error,
          );
          return;
        }
        set((state) => ({
          settings: state.settings
            ? {
                ...state.settings,
                post_process_secret_states: {
                  ...state.settings.post_process_secret_states,
                  [providerId]: result.data,
                },
              }
            : null,
        }));
      } catch (error) {
        console.error("Failed to read provider credential state:", error);
      } finally {
        setUpdating(updateKey, false);
      }
    },

    updatePostProcessModel: async (providerId, model) => {
      const updateKey = `post_process_model:${providerId}`;
      const { setUpdating, refreshSettings } = get();
      setUpdating(updateKey, true);

      try {
        const result = await commands.changePostProcessModelSetting(
          providerId,
          model,
        );
        if (result.status === "error") {
          console.error("Failed to update post-process model:", result.error);
          return;
        }
        await refreshSettings();
      } catch (error) {
        console.error("Failed to update post-process model:", error);
      } finally {
        setUpdating(updateKey, false);
      }
    },

    discoverPostProcessModelCatalog: async (providerId) => {
      const scope = catalogScopeForSettings(get().settings, providerId);
      const updateKey = `post_process_model_catalog:${scope}`;
      const previous = get().postProcessModelCatalogs[scope];
      const providerRevision = catalogProviderRevisions.get(providerId) ?? 0;
      const requestRevision = (catalogRequestRevisions.get(scope) ?? 0) + 1;
      catalogRequestRevisions.set(scope, requestRevision);
      const { setUpdating } = get();

      const canWrite = () =>
        (catalogProviderRevisions.get(providerId) ?? 0) === providerRevision &&
        catalogRequestRevisions.get(scope) === requestRevision &&
        catalogScopeForSettings(get().settings, providerId) === scope;
      const storeCatalog = (catalog: PostProcessModelCatalog) => {
        if (!canWrite()) return;
        set((state) => {
          const current = state.postProcessModelCatalogs[scope];
          return {
            postProcessModelCatalogs: {
              ...state.postProcessModelCatalogs,
              [scope]: {
                catalog,
                cachedModels:
                  catalog.discovery === "ready"
                    ? catalog.models
                    : (current?.cachedModels ?? []),
              },
            },
          };
        });
      };

      setUpdating(updateKey, true);
      try {
        const response =
          await commands.discoverPostProcessModelCatalog(providerId);
        const catalog =
          response.provider_id === providerId
            ? response
            : fallbackCatalog(
                providerId,
                "invalid_response",
                previous?.catalog.allows_manual_model_id ?? true,
              );
        storeCatalog(catalog);
        return catalog;
      } catch {
        const catalog = fallbackCatalog(
          providerId,
          "unreachable",
          previous?.catalog.allows_manual_model_id ?? true,
        );
        storeCatalog(catalog);
        return catalog;
      } finally {
        setUpdating(updateKey, false);
      }
    },

    invalidatePostProcessModelCatalog: (providerId) => {
      catalogProviderRevisions.set(
        providerId,
        (catalogProviderRevisions.get(providerId) ?? 0) + 1,
      );
      const providerPrefix = `${providerId}\u0000`;
      for (const scope of catalogRequestRevisions.keys()) {
        if (scope.startsWith(providerPrefix)) {
          catalogRequestRevisions.delete(scope);
        }
      }
      set((state) => {
        const postProcessModelCatalogs = { ...state.postProcessModelCatalogs };
        for (const scope of Object.keys(postProcessModelCatalogs)) {
          if (scope.startsWith(providerPrefix)) {
            delete postProcessModelCatalogs[scope];
          }
        }
        return { postProcessModelCatalogs };
      });
    },

    // Load default settings from Rust
    loadDefaultSettings: async () => {
      try {
        const result = await commands.getDefaultSettings();
        if (result.status === "ok") {
          set({ defaultSettings: result.data });
        } else {
          console.error("Failed to load default settings:", result.error);
        }
      } catch (error) {
        console.error("Failed to load default settings:", error);
      }
    },

    /* One load per window, however many consumers ask for it.
     *
     * Every `useSettings()` consumer arms this from its mount effect while
     * `isLoading` is still true, and `isLoading` only clears once the first
     * `refreshSettings()` answers — so on a settings page with eighteen
     * consumers this ran eighteen times concurrently. That cost eighteen
     * `get_app_settings`, `get_default_settings` and `check_custom_sounds`
     * round-trips, and it registered the two backend listeners eighteen times
     * over, after which a single `settings-changed` emit fanned out into
     * eighteen refreshes and eighteen store writes.
     *
     * The latch is module state rather than store state because it is not
     * rendered and must not wake a subscriber; the store is a module singleton
     * already. Callers still await the same completion they always did. The body
     * cannot reject — each of the three loads catches its own failure — so
     * there is nothing here to reset and retry. */
    initialize: async () => {
      initialization ??= (async () => {
        const { refreshSettings, checkCustomSounds, loadDefaultSettings } =
          get();

        // Note: Audio devices are NOT refreshed here. The frontend (App.tsx)
        // is responsible for calling refreshAudioDevices/refreshOutputDevices
        // after onboarding completes. This avoids triggering permission dialogs
        // on macOS before the user is ready.
        await Promise.all([
          loadDefaultSettings(),
          refreshSettings(),
          checkCustomSounds(),
        ]);

        // Re-fetch settings when the backend changes them (e.g. language
        // reset during model switch). The backend is the source of truth.
        listen("model-state-changed", () => {
          get().refreshSettings();
        });
        listen<{ setting?: string }>("settings-changed", (event) => {
          get().refreshSettings();
          if (event.payload.setting === "selected_microphone") {
            get().refreshAudioDevices();
          }
        });
      })();

      return initialization;
    },
  })),
);
