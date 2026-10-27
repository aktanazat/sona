import { expect, test, type Locator, type Page } from "@playwright/test";

import { installTauriMock } from "./support/tauri-mock";

/* Keyboard focus must stay visible without pinning its colour or decoration. */

const edgeOf = (field: Locator) =>
  field.evaluate((node) => ({
    focusVisible: node.matches(":focus-visible"),
    borderColor: getComputedStyle(node).borderTopColor,
  }));

/** The meetings list's search field, which arrives with the first meeting.
 * Read under reduced motion: the kit fades a field's edge over 150ms, and a
 * fade in flight reads as whichever colour it is leaving. */
async function meetingsSearchField(page: Page): Promise<Locator> {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await installTauriMock(page, {
    responses: {
      meeting_list: {
        entries: [
          {
            kind: "meeting",
            session_id: "meeting-reviewed",
            title: "Pricing review",
            phase: "review_ready",
            created_at_utc_ms: 1_756_136_400_000,
            capture_completeness: "complete",
            processing_status: { kind: "succeeded" },
            recorded_duration_ms: 1_800_000,
          },
        ],
        has_more: false,
      },
    },
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Meetings", exact: true }).click();
  const field = page.getByRole("searchbox", { name: "Search meetings" });
  await expect(field).toBeVisible();
  return field;
}

test.describe("the focus indicator", () => {
  test("a pressed control shows a visible focus indicator", async ({ page }) => {
    await installTauriMock(page);
    await page.goto("/");
    await expect(
      page.getByRole("navigation", { name: "Main navigation" }),
    ).toBeVisible();

    await page.keyboard.press("Tab");
    const shown = await page.evaluate(() => {
      const node = document.activeElement;
      if (node === null) return { focused: false };
      const style = getComputedStyle(node);
      return {
        focused: node.matches(":focus-visible"),
        marked:
          (style.outlineStyle !== "none" &&
            Number.parseFloat(style.outlineWidth) > 0 &&
            style.outlineColor !== "rgba(0, 0, 0, 0)") ||
          style.boxShadow !== "none",
      };
    });

    expect(shown.focused).toBe(true);
    expect(shown.marked).toBe(true);
  });

  /* A field with a hairline of its own: the kit draws it with a utility, and
   * a rule in the base layer loses to that utility outright, which left the
   * edge grey and the field with no indicator at all. */
  test("a bordered field shows focus in its own edge", async ({ page }) => {
    const field = await meetingsSearchField(page);
    const resting = await edgeOf(field);
    await field.click();

    const edge = await edgeOf(field);
    expect(edge.focusVisible).toBe(true);
    expect(edge.borderColor).not.toBe(resting.borderColor);
  });

});
