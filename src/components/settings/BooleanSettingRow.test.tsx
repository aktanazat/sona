import { describe, expect, test } from "bun:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import type { AppSettings } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { BooleanSettingRow } from "./BooleanSettingRow";

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: {
      translation: {
        row: { label: "Show tray icon", hint: "It stays reachable." },
      },
    },
  },
  interpolation: { escapeValue: false },
});

const paint = (
  settings: AppSettings,
  props: {
    settingKey: "show_tray_icon" | "push_to_talk";
    defaultValue?: boolean;
    hintKey?: string;
  },
  updating: Record<string, boolean> = {},
): string => {
  useSettingsStore.setState({ settings, isUpdating: updating });
  return renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>
        <BooleanSettingRow labelKey="row.label" {...props} />
      </TooltipProvider>
    </I18nextProvider>,
  );
};

/* What the switch reports to assistive tech, which is also the only readable
 * statement of on/off in the markup. */
const state = (markup: string): string =>
  /role="switch" aria-checked="([^"]*)"/.exec(markup)?.[1] ?? "";

describe("a boolean settings row", () => {
  test("reads a stored true as on and a stored false as off", () => {
    expect(
      state(paint({ push_to_talk: true }, { settingKey: "push_to_talk" })),
    ).toBe("true");
    expect(
      state(paint({ push_to_talk: false }, { settingKey: "push_to_talk" })),
    ).toBe("false");
  });

  test("a row that defaults on still reports a stored false as off", () => {
    // `getSetting(k) || true` would answer on here, which is the bug the nine
    // hand-written copies were one edit away from shipping.
    expect(
      state(
        paint(
          { show_tray_icon: false },
          { settingKey: "show_tray_icon", defaultValue: true },
        ),
      ),
    ).toBe("false");
    expect(
      state(paint({}, { settingKey: "show_tray_icon", defaultValue: true })),
    ).toBe("true");
  });

  test("names its switch by the row label, for assistive tech", () => {
    const markup = paint({}, { settingKey: "show_tray_icon" });
    const controlId = /<label for="([^"]+)"/.exec(markup)?.[1];

    expect(markup).toContain("Show tray icon</label>");
    expect(markup).toContain(`id="${controlId ?? "?"}"`);
  });

  test("locks the switch while its write is in flight", () => {
    const markup = paint(
      {},
      { settingKey: "show_tray_icon" },
      {
        show_tray_icon: true,
      },
    );

    expect(
      /<button[^>]*data-slot="switch"[^>]*>/.exec(markup)?.[0] ?? "",
    ).toContain('disabled=""');
  });
});

