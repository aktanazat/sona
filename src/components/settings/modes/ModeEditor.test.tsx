import { afterAll, describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import type {
  AppSettings,
  ModeDefinition,
  ModeView,
  ModelInfo,
  ShortcutBinding,
} from "@/bindings";
import { ModeEditor } from "./ModeEditor";
import { ModesList } from "./ModesList";
import {
  isPresetMode,
  modeDefinitionFromView,
  modeDraftIsDirty,
  modeEngineOptions,
  modeLlmDestination,
} from "./modeModel";

/* Render the mode draft and its eligibility rules without reaching Tauri. */

const localeFile = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localeFile, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

/* `@tauri-apps/plugin-os` reads this synchronously during render, and the
 * shortcut rows go through it. Nothing else in these components needs a DOM.
 * defineProperty, not assignment: under a whole-src run another test file has
 * already planted `window` and plain assignment throws on the readonly slot. */
const priorWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: { __TAURI_OS_PLUGIN_INTERNALS__: { os_type: "macos" } },
});
afterAll(() => {
  if (priorWindow) Object.defineProperty(globalThis, "window", priorWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

const binding = (id: string, chord: string): ShortcutBinding => ({
  id,
  name: id,
  description: id,
  default_binding: chord,
  current_binding: chord,
});

const MODE_VIEW: ModeView = {
  id: "email",
  name: "Email",
  tone: "semi_formal",
  context_policy: "target",
  asr: {
    /* The real shape. `ModelInfo.id` for a catalog model is
     * `repo_id/filename` (managers/model.rs:210-213), not a short slug, and
     * the row has to survive that. Verbatim from
     * src-tauri/src/catalog/catalog.json. */
    model_id:
      "handy-computer/parakeet-tdt-0.6b-v3-gguf/parakeet-tdt-0.6b-v3-Q4_K_M.gguf",
    language: "auto",
    translate_to_english: false,
    custom_words: [{ spoken: "sona", written: "Sona" }],
    filler_word_removal_enabled: true,
    custom_filler_words: null,
    literal_punctuation: false,
    vad_enabled: true,
    requested_engine: "local",
    local_fallback_enabled: true,
    local_fallback_model_id: null,
    cloud_keyterms: [],
    cloud_timestamps: false,
  },
  llm: {
    enabled: true,
    provider_id: "openai",
    model_id: "gpt-4o-mini",
    spoken_instructions: false,
  },
  prompt: { preset: "email", source_prompt_id: null, custom_prompt: null },
  delivery: {
    paste_method: "ctrl_v",
    clipboard_handling: "copy_to_clipboard",
    auto_submit: false,
    auto_submit_key: "enter",
    append_trailing_space: true,
    paste_delay_ms: 40,
    paste_delay_after_ms: 20,
    reliable_paste: true,
    typing_tool: "auto",
    external_script_path: null,
  },
  shortcuts: {
    transcribe: binding("mode/email/transcribe", "option_left+shift+2"),
    switch: binding("mode/email/switch", "option+2"),
  },
};

const MESSAGE_VIEW: ModeView = {
  ...MODE_VIEW,
  id: "message",
  name: "Message",
  shortcuts: {
    transcribe: binding("transcribe", "option+space"),
    switch: binding("mode/message/switch", "option+1"),
  },
};

const MODELS: ModelInfo[] = [];

/* Every window root mounts one `TooltipProvider` and the row primitives assume
 * it, so this stands in for the root. Context only: no markup of its own. */
const render = (node: React.ReactElement): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>{node}</TooltipProvider>
    </I18nextProvider>,
  );

const draft = (overrides: Partial<ModeDefinition> = {}): ModeDefinition => ({
  ...modeDefinitionFromView(MODE_VIEW),
  ...overrides,
});

const noop = () => undefined;

const editor = (
  overrides: Partial<React.ComponentProps<typeof ModeEditor>> = {},
): string =>
  render(
    <ModeEditor
      mode={draft()}
      savedMode={MODE_VIEW}
      modeCount={4}
      models={MODELS}
      onChange={noop}
      onSave={noop}
      saving={false}
      conflict={false}
      activationRules={[]}
      websiteActivationRules={[]}
      activationSupported
      capturingActivation={false}
      onCaptureActivation={noop}
      onRemoveActivation={noop}
      onCaptureWebsiteActivation={noop}
      onRemoveWebsiteActivation={noop}
      {...overrides}
    />,
  );

const expectLabels = (html: string, labels: readonly string[]) => {
  const missing = labels.filter((label) => !html.includes(label));
  expect(missing).toEqual([]);
};

describe("mode editor shell", () => {
  const html = editor();

  test("names the mode, offers Save and keeps the identity field", () => {
    expectLabels(html, [
      "Email",
      "Save changes",
      "Mode name",
      'id="mode-name"',
    ]);
  });

  test("reports an edited draft as unsaved", () => {
    expect(modeDraftIsDirty(draft({ name: "Email drafts" }), MODE_VIEW)).toBe(
      true,
    );
    expect(modeDraftIsDirty(draft(), MODE_VIEW)).toBe(false);
    expect(editor({ mode: draft({ name: "Email drafts" }) })).toContain(
      "Unsaved changes",
    );
  });

  test("blocks Save with a reason when the name is empty", () => {
    const blocked = editor({ mode: draft({ name: "" }) });

    expect(blocked).toContain("A mode name is required.");
    expect(blocked).toMatch(
      /<button\b[^>]*disabled=""[^>]*>Save changes<\/button>/,
    );
  });

  test("surfaces a stale-revision conflict without dropping the draft", () => {
    const conflicted = editor({
      mode: draft({ name: "Email drafts" }),
      conflict: true,
    });

    expect(conflicted).toContain("Settings changed elsewhere");
    expect(conflicted).toContain("Email drafts");
  });
});

describe("instructions", () => {
  test("edits the mode's own rewrite prompt, and says what it overrides", () => {
    const html = editor({
      mode: draft({
        prompt: { ...draft().prompt, custom_prompt: "Keep it to two lines." },
      }),
    });

    expect(html).toContain('id="mode-instructions"');
    expect(html).toContain("Your own instructions");
    expect(html).toContain("Keep it to two lines.");
  });

  test("says why the prompt is inert when cleanup is off", () => {
    const html = editor({
      mode: draft({ llm: { ...draft().llm, enabled: false } }),
    });

    expect(html).toContain("Turn on cleanup");
    expect(html.match(/id="mode-instructions"[^>]*disabled/)).not.toBeNull();
  });
});

describe("model", () => {
  test("keeps the engine, the local model and the language on the screen", () => {
    const html = editor();

    expectLabels(html, [
      "Transcription engine",
      'id="mode-engine"',
      "Transcription model",
      'id="mode-model"',
      "Language",
      'id="mode-language"',
    ]);
  });

  test("hides the local model on a cloud engine and explains the gap", () => {
    const html = editor({
      mode: draft({
        asr: { ...draft().asr, requested_engine: "deepgram_nova_3" },
      }),
    });

    expect(html.includes('id="mode-model"')).toBe(false);
    expect(html).toContain("needs an API key in the system credential store");
  });

  /* The engine menu is a Radix portal: its items exist only once a pointer
   * has opened it, so what it offers is asserted on the model it renders. */
  test("offers both cloud engines and says why one is unusable", () => {
    const options = modeEngineOptions(
      (provider) => provider === "deepgram_nova_3",
      (key, values) => i18n.t(key, values ?? {}),
    );

    expect(options.map((option) => option.value)).toEqual([
      "local",
      "deepgram_nova_3",
      "eleven_labs_scribe_v2",
    ]);
    expect(
      options.filter((option) => option.disabled).map((option) => option.value),
    ).toEqual(["eleven_labs_scribe_v2"]);
  });
});

describe("output", () => {
  test("carries the delivery method and the output style", () => {
    const html = editor();

    expectLabels(html, [
      "Delivery method",
      'id="mode-paste-method"',
      "Output style",
      'id="mode-output-style"',
    ]);
    expect(html.includes('id="mode-external-script"')).toBe(false);
  });

  /* The path is the method's own parameter: a method that cannot run without
   * it must not hide it behind the disclosure. */
  test("reveals the script path beside external-script delivery", () => {
    const html = editor({
      mode: draft({
        delivery: { ...draft().delivery, paste_method: "external_script" },
      }),
    });

    expect(html).toContain('id="mode-external-script"');
    expect(html).toContain("External script");
  });
});

describe("turns on by itself", () => {
  test("lists app and website rules in one list, each with its own remove", () => {
    const html = editor({
      activationRules: [{ app_id: "com.apple.mail", mode_id: "email" }],
      websiteActivationRules: [
        { host: "mail.google.com", match_kind: "suffix", mode_id: "email" },
      ],
    });

    expect(html).toContain('aria-label="Remove com.apple.mail"');
    expect(html).toContain('aria-label="Remove mail.google.com"');
    // The website row's scope is the one thing its host cannot show.
    expect(html).toContain("Host and subdomains");
  });

  test("ignores rules that belong to another mode", () => {
    const html = editor({
      activationRules: [{ app_id: "com.apple.mail", mode_id: "notes" }],
    });

    expect(html).toContain("Nothing switches to this mode on its own.");
    expect(html.includes("com.apple.mail<")).toBe(false);
  });

  test("explains the platform limit instead of hiding the section", () => {
    const html = editor({ activationSupported: false });

    expect(html).toContain("available on macOS");
    expect(html).toContain("Apps and websites");
    expect(html.includes("Capture current app")).toBe(false);
  });

  /* Browser-URL consent is off on a fresh install, so the website half names
   * the switch that turns it on rather than offering a capture that fails. */
  test("points at Privacy when website capture is off", () => {
    const html = editor();

    expect(html).toContain("Include browser URLs");
    expect(html).toContain("in Privacy");
    expect(html.includes("Capture current website")).toBe(false);
  });
});

describe("advanced", () => {
  const html = editor();


  test("holds every knob that survived the collapse", () => {
    expectLabels(html, [
      "Tone",
      "AI provider",
      "AI model",
      "Context level",
      "Translate to English",
      "Literal punctuation",
      "Remove filler words",
      "Voice activity detection",
      "Clipboard",
      "Auto-submit",
      "Submit key",
      "Append a trailing space",
      "Paste delays",
      "Reliable paste",
      "Mode vocabulary",
    ]);
    // The privacy ceiling still outranks the mode, and still says so.
    expect(html).toContain("Privacy currently limits this mode");
    expect(html).toContain("No AI provider is configured");
  });

  /* The cue rides on the mode's rewrite provider, so the row is inert until
   * cleanup is on — and the cue phrase itself has to be discoverable
   * somewhere, which is the hint. The switch renders `aria-checked` and
   * `disabled` before its id, so that is the order these patterns read in. */
  test("offers spoken instructions, off and gated on AI cleanup", () => {
    expect(html).toContain("Spoken instructions");
    expect(html).toMatch(
      /aria-checked="false"[^>]*id="mode-spoken-instructions"/,
    );

    const enabled = editor({
      mode: draft({ llm: { ...draft().llm, spoken_instructions: true } }),
    });
    expect(enabled).toMatch(
      /aria-checked="true"[^>]*id="mode-spoken-instructions"/,
    );

    const noCleanup = editor({
      mode: draft({ llm: { ...draft().llm, enabled: false } }),
    });
    expect(noCleanup).toMatch(/disabled=""[^>]*id="mode-spoken-instructions"/);
  });

  /* These three arrived from the debug pane, where they wrote the global
   * settings root that dictation delivery stopped reading when it moved to
   * per-mode plans. Presence is not the point: the check is that each row
   * shows the draft's own value and moves with it. */
  test("binds the two paste delays and reliable paste to the mode draft", () => {
    expect(html).toContain("40ms");
    expect(html).toContain("20ms");
    expect(html).toMatch(/aria-checked="true"[^>]*id="mode-reliable-paste"/);

    const edited = editor({
      mode: draft({
        delivery: {
          ...draft().delivery,
          paste_delay_ms: 250,
          paste_delay_after_ms: 130,
          reliable_paste: false,
        },
      }),
    });
    expect(edited).toContain("250ms");
    expect(edited).toContain("130ms");
    expect(edited).toMatch(/aria-checked="false"[^>]*id="mode-reliable-paste"/);
    expect(
      modeDraftIsDirty(
        draft({ delivery: { ...draft().delivery, reliable_paste: false } }),
        MODE_VIEW,
      ),
    ).toBe(true);
  });

  test("keeps the per-mode vocabulary editable, including its empty case", () => {
    expect(html).toContain("Add pair");
    expect(html).toContain('aria-label="Remove sona"');

    const empty = editor({
      mode: draft({ asr: { ...draft().asr, custom_words: [] } }),
    });
    expect(empty).toContain("has no vocabulary of its own");
    expect(empty).toContain("Add pair");
  });
});

describe("mode list", () => {
  const list = (selectedModeId: string | null = "email") =>
    render(
      <ModesList
        modes={[MESSAGE_VIEW, MODE_VIEW]}
        activeModeId="message"
        selectedModeId={selectedModeId}
        busy={false}
        osType="macos"
        onSelect={noop}
        onActivate={noop}
        onDuplicate={noop}
        onMove={noop}
        onReorder={noop}
        onRequestDelete={noop}
        onReload={noop}
      />,
    );

  /* A cloud mode nobody shipped, so neither the engine cell nor the preset
   * marker can pass by rendering a constant. */
  const CLOUD_VIEW: ModeView = {
    ...MODE_VIEW,
    id: "cloud",
    name: "Cloud dictation with a deliberately long name",
    asr: { ...MODE_VIEW.asr, requested_engine: "deepgram_nova_3" },
  };

  const single = (mode: ModeView) =>
    render(
      <ModesList
        modes={[mode]}
        activeModeId="message"
        selectedModeId={null}
        busy={false}
        osType="macos"
        onSelect={noop}
        onActivate={noop}
        onDuplicate={noop}
        onMove={noop}
        onReorder={noop}
        onRequestDelete={noop}
        onReload={noop}
      />,
    );

  test("marks the active mode with one word, not a colour", () => {
    const html = list();

    expect(html).toContain("Active");
    // Exactly one row can be active, and one row is open in the editor.
    expect(html.split(">Active<").length - 1).toBe(1);
  });

  test("reads the shipped modes as presets, and only those", () => {
    expect(isPresetMode("message")).toBe(true);
    expect(isPresetMode("notes")).toBe(true);
    expect(isPresetMode("cloud")).toBe(false);

    // Message and Email both ship with Sona, so both rows carry the word.
    expect(list().split(">Preset<").length - 1).toBe(2);
    expect(single(CLOUD_VIEW).includes(">Preset<")).toBe(false);
  });

  test("shows each mode's dictation chord as engraved caps", () => {
    const html = list();

    expect(html).toContain("<kbd");
    // One cap per physical key, in the form macOS engraves.
    expectLabels(html, ["⌥", "⇧", "Space"]);
    expect(html.match(/<kbd/g)?.length).toBe(5);
    // The qualified form is not lost: it is the chord's tooltip.
    expect(html).toContain('title="Left Option + Shift + 2"');
    expect(html).toContain('title="Option + Space"');
  });

  test("carries the engine, and carries it exactly once per row", () => {
    const html = list();

    // Both fixtures run locally, so the word appears once per row and the row
    // does not also print the model, language and delivery the editor below
    // it already owns.
    expect(html.split(">Local<").length - 1).toBe(2);
  });

  test("reads the engine from the mode rather than from a default", () => {
    const html = single(CLOUD_VIEW);

    expect(html).toContain(">Deepgram<");
    expect(html.includes(">Local<")).toBe(false);
  });


  test("reaches each mode's actions by that mode's name", () => {
    const html = list();

    expectLabels(html, [
      'aria-label="Actions for Email"',
      'aria-label="Actions for Message"',
    ]);
    // The menu is a portal: what it offers is asserted on `modeRowActions`
    // in modesReorder.test.tsx, which is also where the reorder lives.
    expect(html.match(/aria-haspopup="menu"/g)?.length).toBe(2);
  });

  test("offers a way out when the list arrives empty", () => {
    const html = render(
      <ModesList
        modes={[]}
        activeModeId="message"
        selectedModeId={null}
        busy={false}
        osType="macos"
        onSelect={noop}
        onActivate={noop}
        onDuplicate={noop}
        onMove={noop}
        onReorder={noop}
        onRequestDelete={noop}
        onReload={noop}
      />,
    );

    expect(html).toContain("No modes are configured.");
    expect(html).toContain("Retry");
  });
});

/* The row that had both facts and stated neither.
 *
 * Every mode in the store that produced this test named `openai` with no key
 * and no consent, while the app was set to a local provider that worked — and
 * the provider row rendered the mode's dead copy without ever mentioning the
 * live one. What follows is the standing proof that a mode which named no
 * provider says which provider it will actually use.
 *
 * The render half runs on the fresh-install store this file already documents:
 * zustand serves `getInitialState` to React's *server* snapshot, so a seeded
 * store never reaches `renderToStaticMarkup` and the configured branch cannot
 * be rendered from here. The two links that branch is made of — resolving the
 * pair, and naming it — are asserted directly instead. */
describe("the mode's rewrite provider row", () => {
  const withProvider = (providerId: string | null): string =>
    editor({
      mode: draft({
        llm: {
          enabled: true,
          provider_id: providerId,
          model_id: "gpt-4o-mini",
          spoken_instructions: false,
        },
      }),
    });

  test("a mode with no provider of its own states that, not a blank", () => {
    const html = withProvider(null);

    /* The trigger is a default with a name, never the `empty` string — which
     * reads "No provider configured" and is the opposite claim. */
    expect(html).toContain(">Default: the global provider</span>");
    expect(html.includes(">No provider configured<")).toBe(false);
    // The model belongs to the inherited provider, so the stale one is gone.
    expect(html.includes("gpt-4o-mini")).toBe(false);
  });

  test("a mode that named its own provider still shows that one", () => {
    const html = withProvider("openai");

    /* No install providers here, so the id is its own label: the row states a
     * choice it cannot resolve rather than quietly dropping it. The warning
     * on a store with no providers at all is the `noneConfigured` one. */
    expect(html).toContain(">openai</span>");
    expect(html).toContain('value="gpt-4o-mini"');
    expect(html).toContain(
      "No AI provider is configured yet. Add one in Settings before this mode can rewrite.",
    );
  });

  test("resolves the pair a mode inherits, and leaves an override alone", () => {
    /* SAFETY: modeLlmDestination reads exactly these two fields, so the
     * fixture carries only them rather than a full AppSettings. Widening the
     * rule to read a third field makes this assertion a lie, which is why the
     * cast is here and not on a helper that would hide it. */
    const global = {
      post_process_provider_id: "custom",
      post_process_models: { custom: "gemma4:12b-mlx", openai: "gpt-4o" },
    } as AppSettings;

    expect(
      modeLlmDestination({ provider_id: null, model_id: "stale" }, global),
    ).toEqual({
      providerId: "custom",
      modelId: "gemma4:12b-mlx",
      inherited: true,
    });
    /* An override is honoured as written even though the global would run and
     * this one is a remote route with no key: the rule reads no credential or
     * consent state, so a decision cannot be undone by its own outage. */
    expect(
      modeLlmDestination(
        { provider_id: "openai", model_id: "gpt-4o-mini" },
        global,
      ),
    ).toEqual({
      providerId: "openai",
      modelId: "gpt-4o-mini",
      inherited: false,
    });
    // A store written before the field existed omits it, which is inherit.
    expect(modeLlmDestination({ model_id: "" }, global).inherited).toBe(true);
  });
});
