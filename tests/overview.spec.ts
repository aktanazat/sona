import { expect, test } from "@playwright/test";

import { installTauriMock } from "./support/tauri-mock";
import { CAPTURE_AT_FULL_HEIGHT } from "./support/tauri-fixtures";

/* Capture ships at 900×800. The weekly detail is closed until asked for, so
 * this path checks that its disclosure still reaches both range controls. */
test.use({ viewport: { width: 900, height: 800 } });

test.describe("the Capture page's weekly detail", () => {
  test("opens through its disclosure and keeps its range controls reachable", async ({
    page,
  }) => {
    await installTauriMock(page, { responses: CAPTURE_AT_FULL_HEIGHT });
    await page.goto("/");

    const week = page.locator("details").filter({ hasText: /^This week/ });
    await expect(week).toBeVisible();
    await week.locator("summary").click();

    await expect(
      week.getByRole("button", { name: "Previous 7 days" }),
    ).toBeVisible();
    await expect(
      week.getByRole("button", { name: "Next 7 days" }),
    ).toBeVisible();
  });
});

test.describe("the Capture page's recording state", () => {
  test("names the state the backend announced", async ({ page }) => {
    /* The mock answers `is_recording` false, so the only way to Listening is
     * the event — which is what this asserts the page is wired to. */
    await installTauriMock(page, {
      responses: CAPTURE_AT_FULL_HEIGHT,
      events: { "dictation-recording-changed-event": [{ recording: true }] },
    });
    await page.goto("/");

    const status = page.locator("#overview-status");
    await expect(status).toHaveText("Listening");
    await expect(status).toHaveAttribute("data-recording", "true");

    /* Nothing asserts the invocation count here: the dev server renders under
     * React's strict mode, so every mount read happens twice. That the page
     * schedules no repeat is asserted where it is deterministic, in
     * src/components/overview/Overview.test.tsx. */
  });
});
