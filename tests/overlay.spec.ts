import { expect, test } from "@playwright/test";

import { APP_SETTINGS } from "./support/tauri-fixtures";
import {
  emitTauriEvent,
  installEventProbe,
  listenerCounts,
} from "./support/tauri-event-probe";
import { installTauriMock } from "./support/tauri-mock";

/* Every native event the overlay webview subscribes to while it is on screen:
 * the seven the HUD registers (src/overlay/overlayEvents.ts:113-127), the
 * appearance follow the route owns (src/app/overlay/page.tsx:30-35) and the
 * theme follow bootstrapWindow adds (src/lib/bootstrapWindow.ts:61-64). The
 * map is named event by event so a new or lost subscription fails on its own
 * name instead of on a total nobody can read.
 *
 * `mic-level` is deliberately absent. It arrived at audio frame rate and its
 * only consumer was the sixteen-bar meter this HUD replaced with a word and a
 * clock; a window that re-renders per frame to draw something nobody reads is
 * the whole reason the idle overlay was never idle. */
const OVERLAY_LISTENERS = {
  "show-overlay": 1,
  "hide-overlay": 1,
  "recording-ready": 1,
  "stream-text-event": 1,
  "stream-phase-event": 1,
  "stream-engine-event": 1,
  "recording-error": 1,
  "settings-changed": 1,
  "theme-changed": 1,
} satisfies Record<string, number>;

test("the overlay route turns native events into one live HUD", async ({
  page,
}) => {
  const overlaySettings = {
    ...APP_SETTINGS,
    appearance_material: "solid",
  };
  await installTauriMock(page, {
    responses: {
      get_app_settings: overlaySettings,
      get_settings: overlaySettings,
    },
  });
  await installEventProbe(page);
  await page.goto("/overlay");

  await expect(page.locator("#root")).toBeVisible();
  /* One listener per event, not two. React StrictMode mounts the appearance
   * effect twice under `next dev`, which is the server Playwright runs, so a
   * cleanup that fails to release `settings-changed` shows up here as a count
   * of 2. A production export mounts once, where this line only states that
   * the subscription exists. */
  await expect.poll(() => listenerCounts(page)).toEqual(OVERLAY_LISTENERS);
  await expect(page.getByRole("status")).toHaveCount(0);

  /* The card is two readouts, so the assertion is its whole text. `Starting`
   * alone is the load-bearing part: the microphone is not open yet, and a
   * clock here would be counting audio nobody captured. */
  const card = page.getByTestId("hud-card");
  await emitTauriEvent(page, "show-overlay", "recording");
  await expect(card).toHaveText("Starting");

  await emitTauriEvent(page, "recording-ready", null);
  await expect(card).toHaveText(/^Listening0:0\d$/);

  /* Cancel is the one control, and it is not part of the resting picture: it
   * is transparent and unclickable until the row is hovered. Both halves
   * matter — a phantom hit target over a readout is worse than no control.
   *
   * The hovered opacity is polled rather than compared: it arrives through a
   * fade, and the last frame of that fade computes as 0.999999, so an equality
   * on the string "1" races the easing curve instead of stating anything about
   * the button. The resting 0 needs no window — nothing is running. */
  const cancel = page.getByRole("button", { name: "Cancel" });
  await expect(cancel).toHaveCSS("opacity", "0");
  await expect(cancel).toHaveCSS("pointer-events", "none");
  await card.hover();
  await expect
    .poll(() =>
      cancel.evaluate((node) => Number(getComputedStyle(node).opacity)),
    )
    .toBeGreaterThan(0.99);
  await expect(cancel).toHaveCSS("pointer-events", "auto");

  await emitTauriEvent(page, "settings-changed", {
    setting: "appearance_material",
    value: "glass",
  });
  await expect(page.locator("html")).toHaveAttribute("data-material", "glass");

  await emitTauriEvent(page, "hide-overlay", null);
  await expect(card).toHaveCount(0);

  /* A failure replaces both readouts with its cause, in the app's own words:
   * the backend's `error_type` token never reaches the screen. And a hide
   * arriving behind it does not yank the window away unread — the cause holds
   * the HUD for its dwell first.
   *
   * Scoped to the card because Next's route announcer is a page-level
   * `role="alert"` of its own. */
  await emitTauriEvent(page, "show-overlay", "recording");
  await emitTauriEvent(page, "recording-error", {
    error_type: "no_speech_detected",
  });
  await expect(card.getByRole("alert")).toHaveText("No speech detected");
  await expect(page.getByRole("status")).toHaveCount(0);

  await emitTauriEvent(page, "hide-overlay", null);
  await expect(card.getByRole("alert")).toHaveText("No speech detected");
});
