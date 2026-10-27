import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import type { ModelInfo } from "@/bindings";
import { useModelStore } from "./modelStore";

/* What a finished download is allowed to do to the reader's choice.
 *
 * The listeners live inside `initialize()`, which is where the defect was, so
 * the host is faked at the Tauri boundary and the real listeners run:
 * `@tauri-apps/api` routes every command, every `listen` registration and every
 * callback transform through `window.__TAURI_INTERNALS__`, so those three are a
 * whole host. Nothing is mocked at module level, which is what makes
 * `initialize()`, the `model-download-complete` handler and its deferred select
 * the code under test rather than a restatement of it.
 *
 * `setTimeout` is stubbed rather than waited out: the 500 ms exists to let the
 * backend release the model files, and a test that slept through it would buy
 * nothing but latency. */

type EventHandler = (event: { payload: string }) => void;

/* The whole value vocabulary the fake host speaks: command replies on the
 * right, argument values on the left. Listen registrations carry the real
 * listener function because `transformCallback` below is the identity. */
type HostArgs = Record<string, string | EventHandler>;
type HostReply = ModelInfo[] | string | number | boolean | null;

/* SAFETY: a four-key partial stands in for ModelInfo because the store only
 * ever reads `id` off these rows; every other field is untouched by the paths
 * under test, so a missing key cannot be dereferenced. */
const CATALOG = ["small", "turbo", "large"].map(
  (id) =>
    ({
      id,
      name: id,
      is_downloaded: true,
      is_downloading: false,
    }) as ModelInfo,
);

const handlers: Record<string, EventHandler> = {};
/** Commands the fake host answered, so a test can assert what was asked. */
let asked: string[] = [];
let activeModel = "small";
let recording = false;

const answer = (command: string, args: HostArgs): HostReply => {
  asked.push(command);
  switch (command) {
    case "plugin:event|listen": {
      const handler = args.handler;
      if (handler instanceof Function) handlers[String(args.event)] = handler;
      return 1;
    }
    case "get_available_models":
      return CATALOG;
    case "get_current_model":
      return activeModel;
    case "set_active_model":
      activeModel = String(args.modelId);
      return null;
    case "download_model":
      return null;
    case "is_recording":
      return recording;
    default:
      throw new Error(`unexpected command: ${command}`);
  }
};

let deferred: (() => void)[] = [];
const priorWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const realSetTimeout = globalThis.setTimeout;

beforeAll(async () => {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: {
      ...globalThis.window,
      __TAURI_INTERNALS__: {
        invoke: (command: string, args: HostArgs = {}) =>
          Promise.resolve(answer(command, args)),
        transformCallback: (callback: EventHandler) => callback,
      },
    },
  });
  /* The store's only timer is the deferred select under test; nothing else in
   * this file schedules one, and the real function is restored below. The
   * widened alias exists because a queue-backed stub cannot satisfy the
   * host's Timer-returning signature. */
  const mutableGlobals: { setTimeout: unknown } = globalThis;
  mutableGlobals.setTimeout = (run: () => void) => {
    deferred.push(run);
    return 0;
  };

  await useModelStore.getState().initialize();
});

afterAll(() => {
  globalThis.setTimeout = realSetTimeout;
  if (priorWindow) Object.defineProperty(globalThis, "window", priorWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

const runDeferredSelect = async () => {
  const scheduled = deferred;
  deferred = [];
  for (const run of scheduled) run();
  /* The deferred body awaits two commands, each an async wrapper over an
   * already-resolved host promise. Draining the microtask queue is what "the
   * timer finished" means with no clock involved; the bound is far above the
   * chain's real depth, and a shortfall would fail an assertion rather than
   * flake. */
  for (let turn = 0; turn < 32; turn += 1) await Promise.resolve();
};

const completeDownload = (modelId: string) => {
  const handler = handlers["model-download-complete"];
  if (!handler)
    throw new Error("initialize() registered no completion handler");
  handler({ payload: modelId });
};

const reset = (current: string) => {
  asked = [];
  deferred = [];
  activeModel = current;
  recording = false;
  useModelStore.setState({
    currentModel: current,
    downloadingModels: {},
    verifyingModels: {},
    downloadProgress: {},
    downloadStats: {},
    activeModelWhenQueued: {},
    error: null,
  });
};

describe("a download that finishes", () => {
  test("becomes the active model when nothing else was chosen", async () => {
    reset("small");
    await useModelStore.getState().downloadModel("turbo");

    completeDownload("turbo");
    await runDeferredSelect();

    expect(useModelStore.getState().currentModel).toBe("turbo");
  });

  test("leaves a model the reader picked mid-download alone", async () => {
    /* The reversion this guard exists for: queue `turbo`, switch to `large`
     * from the catalog while it downloads, and half a second after `turbo`
     * lands the timer used to put `turbo` back, with no notification. */
    reset("small");
    await useModelStore.getState().downloadModel("turbo");
    await useModelStore.getState().selectModel("large");

    completeDownload("turbo");
    await runDeferredSelect();

    expect(useModelStore.getState().currentModel).toBe("large");
    // Not merely reverted late: the switch is never attempted.
    expect(asked.filter((command) => command === "set_active_model")).toEqual([
      "set_active_model",
    ]);
  });

  test("is still adopted when the reader switched away and back", async () => {
    reset("small");
    await useModelStore.getState().downloadModel("turbo");
    await useModelStore.getState().selectModel("large");
    await useModelStore.getState().selectModel("small");

    completeDownload("turbo");
    await runDeferredSelect();

    expect(useModelStore.getState().currentModel).toBe("turbo");
  });

  test("does not interrupt a recording", async () => {
    reset("small");
    await useModelStore.getState().downloadModel("turbo");
    recording = true;

    completeDownload("turbo");
    await runDeferredSelect();

    expect(useModelStore.getState().currentModel).toBe("small");
  });

});
