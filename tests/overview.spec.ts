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
