import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { AppSettings } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { CaptureModeChip, CaptureModePicker } from "./CaptureModeChip";

/**
 * The hero's mode chip.
 *
 * Modes left the sidebar rail, so this is where a mode gets picked. What the
 * chip owes the page: the name of the mode the next dictation runs in, an
 * accessible name that says what pressing it does, and nothing at all before
 * the settings arrive.
 */

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: {
      translation: JSON.parse(
        fs.readFileSync(
          path.join(
            path.dirname(fileURLToPath(import.meta.url)),
            "..",
            "..",
            "i18n",
            "locales",
            "en",
            "translation.json",
          ),
          "utf8",
        ),
      ),
    },
  },
  interpolation: { escapeValue: false },
});

const render = (node: React.ReactElement): string =>
  renderToStaticMarkup(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);

const noop = () => undefined;

/* `modes` on AppSettings is `ModeDefinition[]`, and only the two fields the
 * chip reads matter here — the rest of a mode is the editor's business. */
const MODES = [
  { id: "message", name: "Message" },
  { id: "email", name: "Email" },
  { id: "notes", name: "Notes" },
];

const seed = (settings: AppSettings) => {
  useSettingsStore.setState({ settings, isUpdating: {} });
};

describe("the chip on the hero", () => {
  test("names the current mode, and says what pressing it does", () => {
    seed({
      modes: MODES.map((mode) => ({
        ...mode,
        tone: "balanced",
        context_policy: "none",
        asr: {
          model_id: "",
          language: "auto",
          translate_to_english: false,
          custom_words: [],
          filler_word_removal_enabled: false,
          custom_filler_words: null,
          vad_enabled: true,
        },
        llm: { enabled: false, provider_id: "", model_id: "" },
        prompt: {
          preset: "generic",
          source_prompt_id: null,
          custom_prompt: null,
        },
        delivery: {
          paste_method: "ctrl_v",
          clipboard_handling: "copy_to_clipboard",
          auto_submit: false,
          auto_submit_key: "enter",
          append_trailing_space: false,
          paste_delay_ms: 60,
          paste_delay_after_ms: 60,
          reliable_paste: true,
          typing_tool: "auto",
          external_script_path: null,
        },
      })),
      active_mode_id: "email",
    });

    const markup = render(<CaptureModeChip onOpenModes={noop} />);

    expect(markup).toContain('data-testid="overview-mode-chip"');
    expect(markup).toContain(">Email");
    expect(markup).toContain('aria-label="Change mode, currently Email"');
  });

  /* A chip naming a mode this install does not have would be worse than no
   * chip, so before the settings land it draws nothing. */
  test("draws nothing before the settings arrive", () => {
    seed({});

    expect(render(<CaptureModeChip onOpenModes={noop} />)).toBe("");
  });
});

describe("the picker inside it", () => {
  const html = render(
    <CaptureModePicker
      modes={MODES}
      activeModeId="email"
      busy={false}
      onPick={noop}
      onOpenModes={noop}
    />,
  );

  test("lists every mode as its own control", () => {
    expect(html.match(/<button/g)?.length).toBe(MODES.length + 1);
    for (const mode of MODES) {
      expect(html).toContain(`>${mode.name}<`);
    }
  });

  test("marks exactly the current mode", () => {
    expect(html.match(/aria-current="true"/g)?.length).toBe(1);
  });


  test("locks every pick while a switch is in flight", () => {
    const busy = render(
      <CaptureModePicker
        modes={MODES}
        activeModeId="email"
        busy
        onPick={noop}
        onOpenModes={noop}
      />,
    );

    expect(busy.match(/disabled=""/g)?.length).toBe(MODES.length);
  });
});
