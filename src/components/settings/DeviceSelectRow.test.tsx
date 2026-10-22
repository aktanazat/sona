import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import type { AppSettings, AudioDevice } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { DeviceSelectRow } from "./DeviceSelectRow";

/* The trigger is the only place a device row reports which device is in
 * effect, and it is handed a PERSISTED name against a list enumerated at
 * render time. Those two disagreeing is a routine state, not an error path: a
 * USB mic gets unplugged, an enumeration has not resolved yet, a file the
 * setting still names is gone. What the trigger says in that state is the whole
 * subject of this file, and it is the contract the move from `ui/Dropdown` to
 * bare Radix `Select` silently dropped — Radix portals a label into the trigger
 * only out of a mounted, selected `SelectItem`, so a row that maps just the
 * enumerated devices and passes `SelectValue` no children renders BLANK.
 *
 * These assertions come from the deleted `ui/Dropdown.test.tsx`, which pinned
 * exactly this and went out with the primitive.
 *
 * Seed the loading flag in the server snapshot while painting so a missing
 * in-flight write lock cannot pass just because every control is loading. */

const catalogue = JSON.parse(
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
);

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: catalogue } },
  interpolation: { escapeValue: false },
  parseMissingKeyHandler: () => "__MISSING__",
});

const named = (name: string): AudioDevice => ({
  index: name.toLowerCase(),
  name,
  is_default: name === "Default",
});

const ENUMERATED = [named("Default"), named("MacBook Pro Microphone")];

const paint = (
  settings: AppSettings,
  devices: AudioDevice[] = ENUMERATED,
  extra: { hintKey?: string; disabled?: boolean } = {},
  updating: Record<string, boolean> = {},
): string => {
  useSettingsStore.setState({ settings, isUpdating: updating });
  const snapshot = useSettingsStore.getInitialState();
  const wasLoading = snapshot.isLoading;
  snapshot.isLoading = false;
  try {
    return renderToStaticMarkup(
      <I18nextProvider i18n={i18n}>
        <TooltipProvider>
          <DeviceSelectRow
            settingKey="selected_microphone"
            labelKey="settings.sound.microphone.title"
            devices={devices}
            refresh={() => Promise.resolve()}
            {...extra}
          />
        </TooltipProvider>
      </I18nextProvider>,
    );
  } finally {
    snapshot.isLoading = wasLoading;
  }
};

/* The trigger's value box — the one element that states the device. Empty
 * capture is the regression: a trigger that renders nothing at all. */
const stated = (markup: string): string =>
  /data-slot="select-value"[^>]*>([^<]*)</.exec(markup)?.[1] ?? "";

/* `data-disabled=""` contains `disabled=""`, so the attribute has to be read
 * off one element at a time rather than counted across the row. */
const openingTag = (markup: string, match: string): string =>
  new RegExp(`<button[^>]*${match}[^>]*>`).exec(markup)?.[0] ?? "";

describe("a device row's trigger", () => {
  test("names the device in effect", () => {
    expect(
      stated(paint({ selected_microphone: "MacBook Pro Microphone" })),
    ).toBe("MacBook Pro Microphone");
  });

  test("names a configured device the enumeration does not carry", () => {
    // The device was unplugged; the setting still says Yeti Nano and the row
    // has to keep saying so.
    expect(stated(paint({ selected_microphone: "Yeti Nano" }))).toBe(
      "Yeti Nano",
    );
  });

  test("names it even when nothing has been enumerated at all", () => {
    // Every open of Settings passes through this state: the device list is []
    // until `get_available_microphones` returns.
    expect(stated(paint({ selected_microphone: "Yeti Nano" }, []))).toBe(
      "Yeti Nano",
    );
  });

  test("never falls back to a placeholder, because nothing is never selected", () => {
    /* An empty setting is not "no device": it is the default device, which the
     * backend prepends to every enumeration under the name below. So the row
     * has no unset state to placeholder for, and Radix's own placeholder
     * marker must be absent in every one of these. */
    for (const settings of [
      {},
      { selected_microphone: null },
      { selected_microphone: "" },
      { selected_microphone: "default" },
    ] satisfies AppSettings[]) {
      const markup = paint(settings);

      expect(stated(markup)).toBe("Default");
    }
  });

});

describe("a device row's frame", () => {
  test("labels its control and its reset by the row's own name", () => {
    const markup = paint({ selected_microphone: "Yeti Nano" });
    const controlId = /<label for="([^"]+)"/.exec(markup)?.[1];

    expect(markup).toContain("Microphone</label>");
    // The label points at the Select, not at the reset button beside it.
    expect(markup).toContain(`role="combobox"`);
    expect(markup).toContain(`id="${controlId ?? "?"}"`);
    expect(markup).toContain('aria-label="Reset Microphone"');
  });

  test("an update in flight locks the control AND its reset", () => {
    /* Both, or the user reverts a write that has not landed. This is the one
     * gate the harness can vary — `isUpdatingKey` is a store method, so it
     * reads live where the store's plain values do not. */
    const markup = paint(
      { selected_microphone: "Yeti Nano" },
      ENUMERATED,
      {},
      {
        selected_microphone: true,
      },
    );

    expect(openingTag(markup, 'role="combobox"')).toContain('disabled=""');
    expect(openingTag(markup, 'aria-label="Reset Microphone"')).toContain(
      'disabled=""',
    );
    const idle = paint({ selected_microphone: "Yeti Nano" });
    expect(openingTag(idle, 'role="combobox"')).not.toContain('disabled=""');
    expect(openingTag(idle, 'aria-label="Reset Microphone"')).not.toContain(
      'disabled=""',
    );
  });

});
