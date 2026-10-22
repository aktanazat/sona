import { expect, test } from "@playwright/test";

import { installTauriMock } from "./support/tauri-mock";

test.describe("Sona App", () => {
  test("paints the shell while the model catalog is still pending", async ({
    page,
  }) => {
    await installTauriMock(page, { pending: ["get_available_models"] });
    await page.goto("/");

    await expect
      .poll(() =>
        page.evaluate(() =>
          Number(
            localStorage.getItem("tauri-invoke:get_available_models") ?? "0",
          ),
        ),
      )
      .toBeGreaterThan(0);
    await expect(page.locator('[data-slot="launch-shell"]')).toBeVisible();
  });
});
