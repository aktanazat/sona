import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { installTauriMock, type JsonValue } from "./support/tauri-mock";

/* The recorder dialog, driven through the mocked native runtime.
 *
 * Every case drives the real dialog: setup, preview, record, pause and save,
 * the dismissal that releases the capture, and the palette chord the recorder
 * holds shut.
 *
 * Screen selection is the native picker's, so the mock answers
 * `recorder_preview_start` the way ScreenCaptureKit's picker does: with a
 * previewing snapshot. Nothing in the web layer names a screen.
 */

/* The two permissions the dialog checks with its default inputs: screen
 * always, microphone because that switch ships on. Stated here rather than
 * inherited from the mock's blanket "anything with permission in its name is
 * granted", so the granted case and the denied one read the same way. */
const GRANTED = {
  "plugin:macos-permissions|check_screen_recording_permission": true,
  "plugin:macos-permissions|check_microphone_permission": true,
} satisfies Record<string, JsonValue>;

const SCREEN_DENIED = {
  ...GRANTED,
  "plugin:macos-permissions|check_screen_recording_permission": false,
} satisfies Record<string, JsonValue>;

const SAVED_FILENAME =
  "sona-screen-20260904T101500Z-6f1c9b2e-77d3-4a8f-9a1c-2b7e4d5f6a80.mp4";

const REQUEST_SCREEN =
  "plugin:macos-permissions|request_screen_recording_permission";

const recorder = (page: Page) =>
  page.getByRole("dialog", { name: "Record screen" });

/* The phase, read from the dialog's own live description rather than the
 * header chip: both carry the same sentence, and this is the one a screen
 * reader is handed. A permission notice is a live region too, so this names
 * the description slot instead of every status role in the dialog. */
const phase = (page: Page) =>
  recorder(page).locator('[data-slot="dialog-description"]');

const palette = (page: Page) => page.getByRole("dialog", { name: "Search" });

const openRecorder = async (page: Page) => {
  await page.getByRole("button", { name: "More" }).click();
  await page.getByRole("menuitem", { name: "Record screen" }).click();
  await expect(phase(page)).toHaveText("Set up");
};

const invokeCount = (page: Page, command: string) =>
  page.evaluate(
    (key) => Number(localStorage.getItem(key) ?? "0"),
    `tauri-invoke:${command}`,
  );

test.describe("the screen recorder", () => {
  test("runs a capture from setup to the saved file", async ({ page }) => {
    await installTauriMock(page, { responses: GRANTED });
    await page.goto("/");
    await openRecorder(page);

    const dialog = recorder(page);
    await dialog.getByRole("button", { name: "Choose screen" }).click();
    await expect(phase(page)).toHaveText("Preview ready");
    await expect(dialog.getByText("Screen selected")).toBeVisible();

    await dialog.getByRole("button", { name: "Start recording" }).click();
    await expect(phase(page)).toHaveText("Recording");

    await dialog.getByRole("button", { name: "Pause" }).click();
    await expect(phase(page)).toHaveText("Paused");

    await dialog.getByRole("button", { name: "Resume" }).click();
    await expect(phase(page)).toHaveText("Recording");

    await dialog.getByRole("button", { name: "Stop & save" }).click();
    await expect(phase(page)).toHaveText("Saved");

    /* The file the recorder publishes, named the way it names it, and the one
     * line carrying the native duration and frame size. */
    await expect(dialog.getByText(SAVED_FILENAME)).toBeVisible();
    await expect(dialog.getByText("12s · 1920 × 1080")).toBeVisible();

    await dialog.getByRole("button", { name: "Done" }).click();
    await expect(recorder(page)).toHaveCount(0);
  });

  test("asks for the denied permission before it asks the picker", async ({
    page,
  }) => {
    /* Screen recording denied, everything else granted: the state a Mac is in
     * the first time someone reaches for this dialog. */
    await installTauriMock(page, { responses: SCREEN_DENIED });
    await page.goto("/");
    await openRecorder(page);

    const dialog = recorder(page);
    await dialog.getByRole("button", { name: "Choose screen" }).click();
    await expect(phase(page)).toHaveText("Permission needed");
    await expect(
      dialog.getByText("Allow screen recording to choose a source."),
    ).toBeVisible();

    await dialog.getByRole("button", { name: "Grant access" }).click();
    await expect(
      dialog.getByRole("button", { name: "Open System Settings" }),
    ).toBeVisible();
    expect(await invokeCount(page, REQUEST_SCREEN)).toBe(1);

    /* Re-checking re-reads the answer given in System Settings. It must not
     * ask the OS again — a second request is a second prompt for a permission
     * the user has already been asked about. */
    await dialog.getByRole("button", { name: "Re-check" }).click();
    await expect(
      dialog.getByRole("button", { name: "Open System Settings" }),
    ).toBeVisible();
    expect(await invokeCount(page, REQUEST_SCREEN)).toBe(1);
    expect(await invokeCount(page, "recorder_preview_start")).toBe(0);
  });

  test("cancels the native preview when the dialog is dismissed", async ({
    page,
  }) => {
    await installTauriMock(page, { responses: GRANTED });
    await page.goto("/");
    await openRecorder(page);

    await recorder(page).getByRole("button", { name: "Choose screen" }).click();
    await expect(phase(page)).toHaveText("Preview ready");

    await page.keyboard.press("Escape");
    await expect(recorder(page)).toHaveCount(0);

    /* Dismissing a preview releases the capture, so it is cancel that runs.
     * preview_stop is the "Change" button's command and stays untouched. */
    expect(await invokeCount(page, "recorder_cancel")).toBe(1);
    expect(await invokeCount(page, "recorder_preview_stop")).toBe(0);
  });

  test("holds the command palette shut while the recorder is up", async ({
    page,
  }) => {
    await installTauriMock(page, { responses: GRANTED });
    await page.goto("/");
    await openRecorder(page);

    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toHaveCount(0);
    await expect(recorder(page)).toBeVisible();

    /* And the suppressed chord leaves nothing queued: a palette that opens by
     * itself once the recorder closes is the same bug one step later. */
    await page.keyboard.press("Escape");
    await expect(recorder(page)).toHaveCount(0);
    await expect(palette(page)).toHaveCount(0);

    await page.keyboard.press("Meta+k");
    await expect(palette(page)).toBeVisible();
  });
});
