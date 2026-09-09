import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import type {
  AppSettings,
  MeetingLocalEngine,
  PostProcessModelCatalog,
  PostProcessModelOption,
  SecretState,
} from "@/bindings";
import {
  postProcessModelCatalogScope,
  useSettingsStore,
} from "./settingsStore";

type HostArgs = {
  providerId?: string;
  baseUrl?: string;
  enabled?: boolean;
  engine?: MeetingLocalEngine;
};
type CatalogResponse =
  | PostProcessModelCatalog
  | Promise<PostProcessModelCatalog>;
type HostReply =
  | AppSettings
  | CatalogResponse
  | SecretState
  | { start: boolean; stop: boolean }
  | number
  | null;

const READY_OPTIONS: PostProcessModelOption[] = [
  { id: "gpt-4o-mini", provenance: "provider_reported" },
];
const SECRET_STATE: SecretState = {
  configured: true,
  lastErrorKind: null,
  lastVerifiedAt: null,
};

const settingsFor = (providerId = "openai"): AppSettings => ({
  external_mutations_enabled: true,
  selected_model: "parakeet-v3",
  meeting_local_engine: { kind: "apple_intelligence" },
  meeting_remote_intelligence_enabled: false,
  post_process_provider_id: providerId,
  post_process_models: { [providerId]: "saved-model" },
  post_process_providers: [
    {
      id: "openai",
      label: "OpenAI",
      base_url: "https://api.openai.com/v1",
    },
    {
      id: "custom",
      label: "Custom",
      base_url: "http://localhost:11434/v1",
      allow_base_url_edit: true,
    },
  ],
  post_process_secret_states: {
    openai: { configured: false, lastErrorKind: null, lastVerifiedAt: null },
    custom: { configured: false, lastErrorKind: null, lastVerifiedAt: null },
  },
});

const catalog = (
  providerId: string,
  overrides: Partial<PostProcessModelCatalog> = {},
): PostProcessModelCatalog => ({
  provider_id: providerId,
  models: READY_OPTIONS,
  discovery: "ready",
  allows_manual_model_id: true,
  ...overrides,
});

let settings = settingsFor();
let catalogResponse: () => CatalogResponse = () => catalog("openai");
let asked: string[] = [];
/* Commands the host refuses, and the refusal it sends. Tauri rejects a
 * command that answered `Err(String)` with that plain string rather than an
 * `Error`, which is why the generated bindings hand a backend refusal back as
 * a resolved `{ status: "error" }` instead of throwing. A fixture that
 * rejects with an `Error` exercises a path production never takes. */
const REFUSAL = "policy pins this setting";
let refused = new Set<string>();
const heldCommands = new Map<string, Promise<null>>();
const failedSaves = new Set<string>();
const priorWindow = Object.getOwnPropertyDescriptor(globalThis, "window");

const holdCommand = (command: string): (() => void) => {
  let release: () => void = () => {
    throw new Error("command was not held");
  };
  heldCommands.set(
    command,
    new Promise<null>((resolve) => {
      release = () => resolve(null);
    }),
  );
  return release;
};

const answer = (command: string, args: HostArgs): HostReply => {
  asked.push(command);
  switch (command) {
    case "get_app_settings":
    case "get_default_settings":
      return settings;
    case "check_custom_sounds":
      return { start: false, stop: false };
    case "plugin:event|listen":
      return 1;
    case "discover_post_process_model_catalog":
      return catalogResponse();
    case "set_provider_secret":
    case "delete_provider_secret":
      return SECRET_STATE;
    case "change_post_process_base_url_setting": {
      const { providerId, baseUrl } = args;
      if (providerId === undefined || baseUrl === undefined) {
        throw new Error("provider and base URL are required");
      }
      settings = {
        ...settings,
        post_process_providers: settings.post_process_providers?.map(
          (provider) =>
            provider.id === providerId
              ? { ...provider, base_url: baseUrl }
              : provider,
        ),
      };
      return null;
    }
    case "change_post_process_model_setting":
      return null;
    case "change_external_mutations_enabled_setting":
      settings = { ...settings, external_mutations_enabled: args.enabled };
      return null;
    case "change_meeting_remote_intelligence_enabled_setting":
      settings = {
        ...settings,
        meeting_remote_intelligence_enabled: args.enabled,
      };
      return null;
    case "change_meeting_local_engine_setting":
      settings = { ...settings, meeting_local_engine: args.engine };
      return null;
    default:
      throw new Error(`unexpected command: ${command}`);
  }
};

beforeAll(() => {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      ...globalThis.window,
      __TAURI_INTERNALS__: {
        invoke: async (command: string, args: HostArgs = {}) => {
          const held = heldCommands.get(command);
          if (held) await held;
          if (refused.has(command)) throw REFUSAL;
          const reply = answer(command, args);
          if (failedSaves.has(command)) throw "settings save failed";
          return reply;
        },
        transformCallback: <Callback>(callback: Callback) => callback,
      },
    },
  });
});

afterAll(() => {
  if (priorWindow) Object.defineProperty(globalThis, "window", priorWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

const reset = (providerId = "openai") => {
  asked = [];
  refused = new Set();
  heldCommands.clear();
  failedSaves.clear();
  settings = settingsFor(providerId);
  catalogResponse = () => catalog(providerId);
  useSettingsStore.setState({
    settings,
    defaultSettings: settings,
    isLoading: false,
    isUpdating: {},
    postProcessModelCatalogs: {},
  });
};

describe("post-processing model catalog state", () => {
  test("initialization never discovers a remote model catalog", async () => {
    await useSettingsStore.getState().initialize();

    expect(asked).not.toContain("discover_post_process_model_catalog");
  });

  test("keeps a typed ready response under its provider and endpoint", async () => {
    reset();
    const scope = postProcessModelCatalogScope(
      "openai",
      "https://api.openai.com/v1",
    );

    await useSettingsStore.getState().discoverPostProcessModelCatalog("openai");

    expect(useSettingsStore.getState().postProcessModelCatalogs[scope]).toEqual(
      {
        catalog: catalog("openai"),
        cachedModels: READY_OPTIONS,
      },
    );
  });

  test("keeps the requested provider when a typed catalog names another provider", async () => {
    reset();
    catalogResponse = () => catalog("custom");

    await useSettingsStore.getState().discoverPostProcessModelCatalog("openai");

    const scope = postProcessModelCatalogScope(
      "openai",
      "https://api.openai.com/v1",
    );
    expect(useSettingsStore.getState().postProcessModelCatalogs[scope]).toEqual(
      {
        catalog: {
          provider_id: "openai",
          models: [],
          discovery: "invalid_response",
          allows_manual_model_id: true,
        },
        cachedModels: [],
      },
    );
  });

  test("keeps manual entry open when the first discovery cannot be reached", async () => {
    reset();
    catalogResponse = () => {
      throw new Error("offline");
    };

    const result = await useSettingsStore
      .getState()
      .discoverPostProcessModelCatalog("openai");

    expect(result).toEqual({
      provider_id: "openai",
      models: [],
      discovery: "unreachable",
      allows_manual_model_id: true,
    });
  });

  test("stores a non-ready catalog exactly as the backend returned it", async () => {
    reset();
    catalogResponse = () =>
      catalog("openai", {
        discovery: "missing_credential",
        models: [{ id: "listed-model", provenance: "provider_reported" }],
      });

    await useSettingsStore.getState().discoverPostProcessModelCatalog("openai");

    const scope = postProcessModelCatalogScope(
      "openai",
      "https://api.openai.com/v1",
    );
    expect(
      useSettingsStore.getState().postProcessModelCatalogs[scope]?.catalog,
    ).toEqual(
      catalog("openai", {
        discovery: "missing_credential",
        models: [{ id: "listed-model", provenance: "provider_reported" }],
      }),
    );
  });

  test("keeps the same-config ready list as cached after a failed refresh", async () => {
    reset();
    await useSettingsStore.getState().discoverPostProcessModelCatalog("openai");
    catalogResponse = () => {
      throw new Error("offline");
    };

    await useSettingsStore.getState().discoverPostProcessModelCatalog("openai");

    const scope = postProcessModelCatalogScope(
      "openai",
      "https://api.openai.com/v1",
    );
    const state = useSettingsStore.getState().postProcessModelCatalogs[scope];
    expect(state?.catalog.discovery).toBe("unreachable");
    expect(state?.cachedModels).toEqual(READY_OPTIONS);
  });

  test("does not write a catalog that returned after invalidation", async () => {
    reset();
    let resolveCatalog: (value: PostProcessModelCatalog) => void = () => {
      throw new Error("catalog request did not start");
    };
    catalogResponse = () =>
      new Promise<PostProcessModelCatalog>((resolve) => {
        resolveCatalog = resolve;
      });

    const discovery = useSettingsStore
      .getState()
      .discoverPostProcessModelCatalog("openai");
    useSettingsStore.getState().invalidatePostProcessModelCatalog("openai");
    resolveCatalog(catalog("openai"));
    await discovery;

    expect(useSettingsStore.getState().postProcessModelCatalogs).toEqual({});
  });

  test("does not let an older failure replace a newer catalog", async () => {
    reset();
    let rejectCatalog: (error: Error) => void = () => {
      throw new Error("catalog request did not start");
    };
    catalogResponse = () =>
      new Promise<PostProcessModelCatalog>((_resolve, reject) => {
        rejectCatalog = reject;
      });

    const olderDiscovery = useSettingsStore
      .getState()
      .discoverPostProcessModelCatalog("openai");
    const newerCatalog = catalog("openai", {
      models: [{ id: "gpt-4.1-mini", provenance: "provider_reported" }],
    });
    catalogResponse = () => newerCatalog;
    await useSettingsStore.getState().discoverPostProcessModelCatalog("openai");
    rejectCatalog(new Error("offline"));
    await olderDiscovery;

    const scope = postProcessModelCatalogScope(
      "openai",
      "https://api.openai.com/v1",
    );
    expect(useSettingsStore.getState().postProcessModelCatalogs[scope]).toEqual(
      {
        catalog: newerCatalog,
        cachedModels: newerCatalog.models,
      },
    );
  });
  test("invalidates a provider catalog when its credential changes", async () => {
    reset();
    await useSettingsStore.getState().discoverPostProcessModelCatalog("openai");

    await useSettingsStore.getState().replacePostProcessSecret("openai", "key");

    expect(
      Object.keys(useSettingsStore.getState().postProcessModelCatalogs),
    ).not.toContain(
      postProcessModelCatalogScope("openai", "https://api.openai.com/v1"),
    );
  });

  test("invalidates every custom-endpoint catalog when its base URL changes", async () => {
    reset("custom");
    await useSettingsStore.getState().discoverPostProcessModelCatalog("custom");

    await useSettingsStore
      .getState()
      .updatePostProcessBaseUrl("custom", "http://localhost:11435/v1");

    expect(
      Object.keys(useSettingsStore.getState().postProcessModelCatalogs).some(
        (scope) => scope.startsWith("custom\u0000"),
      ),
    ).toBe(false);
  });
});

/* Every row on Settings reads its value out of this store, and a write is
 * shown before it lands. What the store does with a refusal is therefore what
 * the switch claims: the consent rows on Agents are the sharp end of it - a
 * grant a reader believes withdrawn, because the switch moved, while the
 * backend still holds it. */
describe("a write the backend refuses", () => {
  test("leaves a consent row reading the grant the backend still holds", async () => {
    reset();
    refused.add("change_external_mutations_enabled_setting");
    refused.add("get_app_settings");
    useSettingsStore.setState({
      settings: { ...settingsFor(), external_mutations_enabled: true },
    });

    await useSettingsStore
      .getState()
      .updateSetting("external_mutations_enabled", false);

    expect(
      useSettingsStore.getState().settings?.external_mutations_enabled,
    ).toBe(true);
  });

  test("puts back the provider without waiting on a second read to correct it", async () => {
    reset();
    refused.add("set_post_process_provider");
    /* The read that would otherwise paper over the failed write is out too,
     * which is the state a backend in trouble is actually in. */
    refused.add("get_app_settings");

    await useSettingsStore.getState().setPostProcessProvider("custom");

    expect(useSettingsStore.getState().settings?.post_process_provider_id).toBe(
      "openai",
    );
  });

  /* A key no command writes is the same lie by a different route: the row
   * moves, nothing is sent, and the store is the only place the new value
   * exists. */
  test("puts back a key no settings command writes", async () => {
    reset();
    useSettingsStore.setState({
      settings: { ...settingsFor(), selected_model: "parakeet-v3" },
    });

    await useSettingsStore
      .getState()
      .updateSetting("selected_model", "whisper-large-v3");

    expect(useSettingsStore.getState().settings?.selected_model).toBe(
      "parakeet-v3",
    );
  });
});

describe("local engine save failures", () => {
  const engine: MeetingLocalEngine = {
    kind: "local_endpoint",
    base_url: "http://127.0.0.1:11434/v1",
    model: "fixture-model",
    context_window_tokens: 8192,
  };
  const command = "change_meeting_local_engine_setting";

  test.each([false, true])(
    "preserves concurrent remote consent %p after local refusal",
    async (enabled) => {
      reset();
      settings = {
        ...settings,
        meeting_remote_intelligence_enabled: !enabled,
      };
      useSettingsStore.setState({ settings });
      const release = holdCommand(command);
      const pending = useSettingsStore
        .getState()
        .updateSetting("meeting_local_engine", engine);

      await useSettingsStore
        .getState()
        .updateSetting("meeting_remote_intelligence_enabled", enabled);
      refused.add(command);
      release();
      await pending;

      expect(
        useSettingsStore
          .getState()
          .getSetting("meeting_remote_intelligence_enabled"),
      ).toBe(enabled);
      expect(
        useSettingsStore.getState().getSetting("meeting_local_engine"),
      ).toEqual({
        kind: "apple_intelligence",
      });
      await useSettingsStore.getState().refreshSettings();
      expect(
        useSettingsStore
          .getState()
          .getSetting("meeting_remote_intelligence_enabled"),
      ).toBe(enabled);
    },
  );

  test("preserves an in-flight consent change during local readback", async () => {
    reset();
    const release = holdCommand(
      "change_meeting_remote_intelligence_enabled_setting",
    );
    const pending = useSettingsStore
      .getState()
      .updateSetting("meeting_remote_intelligence_enabled", true);
    refused.add(command);

    try {
      await useSettingsStore
        .getState()
        .updateSetting("meeting_local_engine", engine);
      expect(
        useSettingsStore
          .getState()
          .getSetting("meeting_remote_intelligence_enabled"),
      ).toBe(true);
    } finally {
      release();
      await pending;
    }

    await useSettingsStore.getState().refreshSettings();
    expect(
      useSettingsStore
        .getState()
        .getSetting("meeting_remote_intelligence_enabled"),
    ).toBe(true);
  });

  test("shows the effective engine when its disk save failed", async () => {
    reset();
    failedSaves.add(command);

    await useSettingsStore
      .getState()
      .updateSetting("meeting_local_engine", engine);

    expect(
      useSettingsStore.getState().getSetting("meeting_local_engine"),
    ).toEqual(engine);
    await useSettingsStore.getState().refreshSettings();
    expect(
      useSettingsStore.getState().getSetting("meeting_local_engine"),
    ).toEqual(engine);
  });
});
