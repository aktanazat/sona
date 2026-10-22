import { afterAll, describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { ModeView, ShortcutBinding } from "@/bindings";
import { ModesList } from "./ModesList";
import {
  modeRowActions,
  orderWithMove,
  type ModeRowActionId,
} from "./modeModel";

/* The mode list can be reordered two ways and both have to land the same value
 * in the same command. The drag is a browser fact and is asserted end to end in
 * tests/motion.spec.ts; what is provable here is the other half: the order the
 * keyboard route produces, and that the list still renders in full before the
 * drag code has even loaded. */

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

const priorWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: { __TAURI_OS_PLUGIN_INTERNALS__: { os_type: "macos" } },
});
afterAll(() => {
  if (priorWindow) Object.defineProperty(globalThis, "window", priorWindow);
  else Reflect.deleteProperty(globalThis, "window");
});

const ORDER = ["message", "email", "meeting", "notes"];

describe("orderWithMove", () => {
  test("moving down swaps with the next mode", () => {
    expect(orderWithMove(ORDER, "email", 1)).toEqual([
      "message",
      "meeting",
      "email",
      "notes",
    ]);
  });

  test("moving up swaps with the previous mode", () => {
    expect(orderWithMove(ORDER, "meeting", -1)).toEqual([
      "message",
      "meeting",
      "email",
      "notes",
    ]);
  });

  /* The menu items at either end are disabled, so this is the belt: a move that
   * would leave the list returns the list, and the caller's equality check then
   * skips the command entirely. */
  test("a move off either end changes nothing", () => {
    expect(orderWithMove(ORDER, "message", -1)).toEqual(ORDER);
    expect(orderWithMove(ORDER, "notes", 1)).toEqual(ORDER);
  });

  test("an unknown mode changes nothing", () => {
    expect(orderWithMove(ORDER, "gone", 1)).toEqual(ORDER);
  });

  test("moving a mode does not mutate the saved order", () => {
    const source = [...ORDER];
    orderWithMove(source, "email", 1);
    expect(source).toEqual(ORDER);
  });

});

const binding = (chord: string): ShortcutBinding => ({
  id: "transcribe",
  name: "Transcribe",
  description: "Transcribe",
  default_binding: chord,
  current_binding: chord,
});

const mode = (id: string, name: string): ModeView => ({
  id,
  name,
  tone: "balanced",
  context_policy: "target",
  asr: {
    model_id: "",
    language: "en",
    translate_to_english: false,
    custom_words: [],
    filler_word_removal_enabled: false,
    custom_filler_words: null,
    vad_enabled: true,
    requested_engine: "local",
    local_fallback_enabled: true,
    local_fallback_model_id: null,
  },
  llm: { enabled: false, provider_id: "openai", model_id: "" },
  prompt: {
    preset: "minimalist_cleanup",
    source_prompt_id: null,
    custom_prompt: "",
  },
  delivery: {
    paste_method: "ctrl_v",
    clipboard_handling: "copy_to_clipboard",
    auto_submit: false,
    auto_submit_key: "enter",
    append_trailing_space: false,
    paste_delay_ms: 0,
    paste_delay_after_ms: 0,
    reliable_paste: false,
    typing_tool: "auto",
    external_script_path: null,
  },
  shortcuts: {
    transcribe: binding("cmd+shift+space"),
    switch: binding(""),
  },
});

const MODES = ORDER.map((id) => mode(id, id.toUpperCase()));

const noop = () => undefined;

const list = () =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <ModesList
        modes={MODES}
        activeModeId="message"
        selectedModeId="email"
        busy={false}
        osType="macos"
        onSelect={noop}
        onActivate={noop}
        onDuplicate={noop}
        onMove={noop}
        onReorder={noop}
        onRequestDelete={noop}
        onReload={noop}
      />
    </I18nextProvider>,
  );

/* The draggable list is an async chunk. Until it lands, the Suspense fallback
 * is what a user sees, so it has to be the whole list rather than a spinner —
 * and it is the only list a keyboard user ever needs. */
describe("the list before the drag code loads", () => {
  const markup = list();

  test("every mode is a row, in the order given", () => {
    const positions = ORDER.map((id) => markup.indexOf(id.toUpperCase()));
    expect(positions.every((at) => at >= 0)).toBe(true);
    expect([...positions].sort((a, b) => a - b)).toEqual(positions);
  });

  test("the rows are the same list element the draggable one renders", () => {
    /* The heading is not drawn — the page title and the view tab both already
     * say Modes — so the list carries its name as an accessible one. */
    expect(markup).toContain('<ul aria-label="Your modes"');
    expect(markup.split("<li").length - 1).toBe(MODES.length);
  });

  test("the selected row still says so", () => {
    expect(markup.split('aria-current="true"').length - 1).toBe(1);
  });

  test("every row reaches its own actions by name", () => {
    for (const modeView of MODES) {
      expect(markup).toContain(`aria-label="Actions for ${modeView.name}"`);
    }
  });
});

/* The menu itself is a Radix portal, so what a row offers is proved on the
 * model the menu renders rather than on markup that only exists once a pointer
 * has opened it. This is the keyboard route to a reorder — the only route
 * without a pointer — so it is asserted position by position. */
describe("modeRowActions", () => {
  const actionsAt = (index: number, overrides: { busy?: boolean } = {}) =>
    modeRowActions(MODES[index], {
      index,
      count: MODES.length,
      isActive: MODES[index].id === "message",
      busy: overrides.busy ?? false,
      t: (_key, fallback) => fallback,
    });

  const byId = (index: number, id: ModeRowActionId) =>
    actionsAt(index).find((action) => action.id === id);

  test("a plain row offers all five actions", () => {
    expect(actionsAt(1).map((action) => action.id)).toEqual([
      "activate",
      "duplicate",
      "moveUp",
      "moveDown",
      "delete",
    ]);
  });

  test("the active mode has nothing to activate, so the item is absent", () => {
    expect(actionsAt(0).some((action) => action.id === "activate")).toBe(false);
  });

  test("the first row cannot move up and the last cannot move down", () => {
    expect(byId(0, "moveUp")?.disabled).toBe(true);
    expect(byId(0, "moveDown")?.disabled).toBe(false);
    const last = MODES.length - 1;
    expect(byId(last, "moveDown")?.disabled).toBe(true);
    expect(byId(last, "moveUp")?.disabled).toBe(false);
  });

  test("the default mode refuses deletion and says why", () => {
    /* `message` is DEFAULT_MODE_ID: the item stays, so the reason is
     * readable, and it is the label that carries it rather than a tooltip on
     * a control that has vanished. */
    const remove = byId(0, "delete");
    expect(remove?.disabled).toBe(true);
    expect(remove?.label).toBe("The default mode cannot be deleted.");
    expect(byId(1, "delete")?.label).toBe("Delete");
  });

  test("delete is the only destructive item", () => {
    expect(
      actionsAt(1)
        .filter((action) => action.destructive)
        .map((action) => action.id),
    ).toEqual(["delete"]);
  });

  test("a mutation in flight disables every revisioned action", () => {
    expect(
      actionsAt(1, { busy: true }).every((action) => action.disabled),
    ).toBe(true);
  });
});
