import { describe, expect, test } from "bun:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { AppSettings } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { SoundPicker } from "./SoundPicker";

/* Custom is OFFERED only once both custom sound files exist, but it can
 * already be the SAVED value — `customSounds` starts `{ start: false, stop:
 * false }` on every boot until the file check resolves, and a file can be
 * deleted later while the setting still reads `custom`. Radix portals a label
 * into the trigger only out of a mounted, selected item, so in exactly those
 * two states the row used to render blank: no theme named, on a row whose only
 * job is to name the theme.
 *
 * `customSounds` here is the store's initial `{ start: false, stop: false }` —
 * zustand serves `getServerSnapshot` under `renderToStaticMarkup`, which
 * happens to be the state under test. */

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: {} } },
  interpolation: { escapeValue: false },
});

const stated = (settings: AppSettings): string => {
  useSettingsStore.setState({ settings, isUpdating: {} });
  const markup = renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <SoundPicker label="Sound theme" />
    </I18nextProvider>,
  );
  return /data-slot="select-value"[^>]*>([^<]*)</.exec(markup)?.[1] ?? "";
};

describe("the sound theme row", () => {
  test("names a saved theme the list is not offering", () => {
    expect(stated({ sound_theme: "custom" })).toBe("Custom");
  });

  test("names an offered theme", () => {
    expect(stated({ sound_theme: "pop" })).toBe("Pop");
  });

});
