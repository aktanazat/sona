import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { APP_SETTINGS } from "./support/tauri-fixtures";
import {
  emitTauriEvent,
  eventListenerCount,
  installEventProbe,
} from "./support/tauri-event-probe";
import { installTauriMock } from "./support/tauri-mock";

const DEBUG_SETTINGS = {
  ...APP_SETTINGS,
  always_on_microphone: false,
  debug_mode: true,
  extra_recording_buffer_ms: 0,
  log_level: "debug",
  sound_theme: "marimba",
  word_correction_threshold: 0.18,
};

const sidebarNav = (page: Page) =>
  page.getByRole("navigation", { name: "Main navigation" });

const settingsTabs = (page: Page) =>
  page.getByRole("tablist", { name: "Settings" });

const commandCount = (page: Page, command: string) =>
  page.evaluate(
    (commandName) =>
      Number(localStorage.getItem(`tauri-invoke:${commandName}`) ?? "0"),
    command,
  );

/* Each opener installs the mock on a page that has not navigated yet, because
 * `installTauriMock` is an init script: running two of them against one page
 * leaves the globals to init-script ordering. */
const openSettings = async (page: Page, settings = APP_SETTINGS) => {
  await installTauriMock(page, {
    responses: {
      get_app_settings: settings,
      get_settings: settings,
      get_available_microphones: [
        { index: "built-in", is_default: true, name: "MacBook Microphone" },
      ],
      is_laptop: true,
    },
  });
  await page.goto("/");
  await sidebarNav(page)
    .getByRole("button", { name: "Settings", exact: true })
    .click();
  await expect(page.getByTestId("settings-hub")).toBeVisible();
};

const openDebug = async (
  page: Page,
  rejectedCommands: readonly string[] = [],
) => {
  await installTauriMock(page, {
    responses: {
      get_app_settings: DEBUG_SETTINGS,
      get_settings: DEBUG_SETTINGS,
      get_available_microphones: [
        { index: "built-in", is_default: true, name: "MacBook Microphone" },
      ],
      is_laptop: true,
    },
  });
  await installEventProbe(page, { rejectedCommands });
  await page.goto("/");
  await sidebarNav(page)
    .getByRole("button", { name: "Settings", exact: true })
    .click();
  await page.getByRole("tab", { name: "Debug", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Debug", exact: true }),
  ).toBeVisible();
};

const choose = async (page: Page, label: string, value: string) => {
  await page.getByRole("combobox", { name: label, exact: true }).click();
  await page.getByRole("option", { name: value, exact: true }).click();
};

test.describe("Debug settings", () => {
  test("a published build has no Debug route while debug mode is off", async ({
    page,
  }) => {
    await openSettings(page);

    await expect(settingsTabs(page).getByRole("tab")).toHaveText([
      "Essentials",
      "Advanced",
      "Prompts",
    ]);
    await expect(
      page.getByRole("heading", { name: "Debug", exact: true }),
    ).toHaveCount(0);
  });

  test("debug mode adds the Debug tab and opens it", async ({ page }) => {
    await openDebug(page);

    await expect(settingsTabs(page).getByRole("tab")).toHaveText([
      "Essentials",
      "Advanced",
      "Prompts",
      "Debug",
    ]);
    await expect(
      settingsTabs(page).getByRole("tab", { name: "Debug", exact: true }),
    ).toHaveAttribute("aria-selected", "true");
  });

  test("every Debug row writes its setting through to the backend", async ({
    page,
  }) => {
    await openDebug(page);

    const alwaysOn = page.getByRole("switch", {
      name: "Always-on microphone",
      exact: true,
    });
    await alwaysOn.click();
    await expect(alwaysOn).toHaveAttribute("aria-checked", "true");
    await expect
      .poll(() => commandCount(page, "update_microphone_mode"))
      .toBe(1);

    await choose(page, "Clamshell microphone", "MacBook Microphone");
    await expect
      .poll(() => commandCount(page, "set_clamshell_microphone"))
      .toBe(1);

    const recordingBuffer = page.getByRole("slider", {
      name: "Extra recording buffer",
      exact: true,
    });
    await recordingBuffer.press("ArrowRight");
    await expect(page.getByText("50ms", { exact: true })).toBeVisible();
    await expect
      .poll(() => commandCount(page, "change_extra_recording_buffer_setting"))
      .toBe(1);

    await choose(page, "Sound theme", "Pop");
    await expect
      .poll(() => commandCount(page, "change_sound_theme_setting"))
      .toBe(1);

    const threshold = page.getByRole("slider", {
      name: "Word correction threshold",
      exact: true,
    });
    await threshold.press("ArrowRight");
    await expect(page.getByText("0.19", { exact: true })).toBeVisible();
    await expect
      .poll(() =>
        commandCount(page, "change_word_correction_threshold_setting"),
      )
      .toBe(1);

    await choose(page, "Log level", "Trace");
    await expect.poll(() => commandCount(page, "set_log_level")).toBe(1);
  });

  test("a refused keyboard diagnostic is reported in the user's language", async ({
    page,
  }) => {
    await openDebug(page, ["run_keyboard_diagnostic"]);

    await page
      .getByRole("button", { name: "Run 10s diagnostic", exact: true })
      .click();

    const diagnosticError = page.getByRole("status");
    await expect(diagnosticError).toHaveText(
      "Sona cannot read keyboard events. Allow Input Monitoring, then try again.",
    );
    await expect(diagnosticError).not.toContainText("permission_denied");
  });

  test("the release note preview opens and closes", async ({ page }) => {
    await openDebug(page);

    await expect(
      page.getByText("Preview the release note", { exact: true }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Open", exact: true }).click();
    await expect(page.getByRole("dialog")).toBeVisible();

    await page.getByRole("button", { name: "Close", exact: true }).click();
    await expect(page.getByRole("dialog")).toHaveCount(0);
  });

  test("leaving Debug releases the live log stream and returning re-creates it", async ({
    page,
  }) => {
    await openDebug(page);

    await expect.poll(() => eventListenerCount(page, "log://log")).toBe(1);
    await emitTauriEvent(page, "log://log", {
      level: 4,
      message: "first line",
    });
    await expect(page.getByText("first line", { exact: true })).toHaveCount(1);

    await page.getByRole("tab", { name: "Essentials", exact: true }).click();
    await expect.poll(() => eventListenerCount(page, "log://log")).toBe(0);

    await page.getByRole("tab", { name: "Debug", exact: true }).click();
    await expect.poll(() => eventListenerCount(page, "log://log")).toBe(1);

    await emitTauriEvent(page, "log://log", {
      level: 5,
      message: "second line",
    });
    await expect(page.getByText("second line", { exact: true })).toHaveCount(1);
    await expect(page.getByText("first line", { exact: true })).toHaveCount(0);
  });
});
