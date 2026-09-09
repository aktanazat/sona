import { useEffect } from "react";
import { useShallow } from "zustand/react/shallow";
import {
  useSettingsStore,
  type PostProcessModelCatalogState,
  type SettingsStore,
  type SettingValue,
} from "../stores/settingsStore";
import type { AppSettings as Settings, AudioDevice } from "@/bindings";

interface UseSettingsReturn {
  // State
  settings: Settings | null;
  defaultSettings: Settings | null;
  isLoading: boolean;
  isUpdating: (key: string) => boolean;
  audioDevices: AudioDevice[];
  outputDevices: AudioDevice[];
  audioFeedbackEnabled: boolean;
  postProcessModelCatalogs: Record<string, PostProcessModelCatalogState>;

  // Actions
  updateSetting: <K extends keyof Settings>(
    key: K,
    value: SettingValue<K>,
  ) => Promise<void>;
  resetSetting: (key: keyof Settings) => Promise<void>;
  refreshSettings: () => Promise<void>;
  refreshAudioDevices: () => Promise<void>;
  refreshOutputDevices: () => Promise<void>;

  // Binding-specific actions
  updateBinding: (id: string, binding: string) => Promise<void>;
  resetBinding: (id: string) => Promise<void>;

  // Convenience getters
  getSetting: <K extends keyof Settings>(key: K) => Settings[K] | undefined;
  getDefaultSetting: <K extends keyof Settings>(
    key: K,
  ) => Settings[K] | undefined;

  // Post-processing helpers
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
  ) => Promise<import("@/bindings").PostProcessModelCatalog>;
  invalidatePostProcessModelCatalog: (providerId: string) => void;
}

/* Exactly the slices this hook hands back, named one by one, so the list below
 * is the whole truth about what wakes a consumer.
 *
 * The store also holds customSounds, which nothing here exposes; a write to it
 * no longer wakes every settings consumer to re-read a value none of them can see.
 *
 * `isUpdating` names the record, not the `isUpdatingKey` getter callers invoke:
 * the getter reads through `get()`, so the subscription that makes a control's
 * spinner appear and clear has to name the record it reads.
 *
 * Every slice is returned by reference and never mapped, spread or rebuilt, so
 * `settings` keeps the identity its callers depend on — Overview hangs
 * `useCallback` deps off `settings.modes` and a listener off that identity. The
 * actions are created once by `create()` and never replaced, so they sit in the
 * comparison without ever moving it. */
const selectExposed = (state: SettingsStore) => ({
  settings: state.settings,
  defaultSettings: state.defaultSettings,
  isLoading: state.isLoading,
  isUpdating: state.isUpdating,
  audioDevices: state.audioDevices,
  outputDevices: state.outputDevices,
  postProcessModelCatalogs: state.postProcessModelCatalogs,
  isUpdatingKey: state.isUpdatingKey,
  getSetting: state.getSetting,
  getDefaultSetting: state.getDefaultSetting,
  initialize: state.initialize,
  updateSetting: state.updateSetting,
  resetSetting: state.resetSetting,
  refreshSettings: state.refreshSettings,
  refreshAudioDevices: state.refreshAudioDevices,
  refreshOutputDevices: state.refreshOutputDevices,
  updateBinding: state.updateBinding,
  resetBinding: state.resetBinding,
  setPostProcessProvider: state.setPostProcessProvider,
  updatePostProcessBaseUrl: state.updatePostProcessBaseUrl,
  replacePostProcessSecret: state.replacePostProcessSecret,
  removePostProcessSecret: state.removePostProcessSecret,
  refreshPostProcessSecretState: state.refreshPostProcessSecretState,
  updatePostProcessModel: state.updatePostProcessModel,
  discoverPostProcessModelCatalog: state.discoverPostProcessModelCatalog,
  invalidatePostProcessModelCatalog: state.invalidatePostProcessModelCatalog,
});

export const useSettings = (): UseSettingsReturn => {
  const store = useSettingsStore(useShallow(selectExposed));

  // Initialize on first mount
  useEffect(() => {
    if (store.isLoading) {
      store.initialize();
    }
  }, [store.initialize, store.isLoading]);

  return {
    settings: store.settings,
    defaultSettings: store.defaultSettings,
    isLoading: store.isLoading,
    isUpdating: store.isUpdatingKey,
    audioDevices: store.audioDevices,
    outputDevices: store.outputDevices,
    audioFeedbackEnabled: store.settings?.audio_feedback || false,
    postProcessModelCatalogs: store.postProcessModelCatalogs,
    updateSetting: store.updateSetting,
    resetSetting: store.resetSetting,
    refreshSettings: store.refreshSettings,
    refreshAudioDevices: store.refreshAudioDevices,
    refreshOutputDevices: store.refreshOutputDevices,
    updateBinding: store.updateBinding,
    resetBinding: store.resetBinding,
    getSetting: store.getSetting,
    getDefaultSetting: store.getDefaultSetting,
    setPostProcessProvider: store.setPostProcessProvider,
    updatePostProcessBaseUrl: store.updatePostProcessBaseUrl,
    replacePostProcessSecret: store.replacePostProcessSecret,
    removePostProcessSecret: store.removePostProcessSecret,
    refreshPostProcessSecretState: store.refreshPostProcessSecretState,
    updatePostProcessModel: store.updatePostProcessModel,
    discoverPostProcessModelCatalog: store.discoverPostProcessModelCatalog,
    invalidatePostProcessModelCatalog: store.invalidatePostProcessModelCatalog,
  };
};
