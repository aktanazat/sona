import { expect, test, type Locator, type Page } from "@playwright/test";

import { installTauriMock } from "./support/tauri-mock";

/* Two focus indicators, each owning a kind of control: something you press
 * shows a 2px bronze indicator around it, something you type in shows its own
 * edge and nothing around it. The second rule exists because a text field
 * matches :focus-visible on a plain click, so the offset outline read as a
 * second box drawn around the box being typed in — on the chat composer,
 * whose wrapper already darkens its hairline, two boxes at once. */
const BRONZE = "rgb(139, 90, 43)";
/* The theme's destructive red, which an invalid field wears at rest. */
const RED = "rgb(229, 72, 77)";

const edgeOf = (field: Locator) =>
  field.evaluate((node) => ({
    focusVisible: node.matches(":focus-visible"),
    borderColor: getComputedStyle(node).borderTopColor,
    outlineStyle: getComputedStyle(node).outlineStyle,
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
  test("a pressed control shows a bronze indicator", async ({ page }) => {
    await installTauriMock(page);
    await page.goto("/");
    await expect(
      page.getByRole("navigation", { name: "Main navigation" }),
    ).toBeVisible();

    await page.keyboard.press("Tab");
    const shown = await page.evaluate((bronze) => {
      const node = document.activeElement;
      if (node === null) return { focused: false };
      const style = getComputedStyle(node);
      return {
        focused: node.matches(":focus-visible"),
        tag: node.tagName,
        /* Either treatment counts, and both are the same colour: the app-wide
         * outline from base.css, or a component's own ring, which the kit
         * draws as a box-shadow. */
        marked:
          (style.outlineStyle !== "none" && style.outlineColor === bronze) ||
          style.boxShadow.includes(bronze),
      };
    }, BRONZE);

    expect(shown.focused).toBe(true);
    expect(shown.marked).toBe(true);
  });

  test("a typed field shows its own edge and nothing around it", async ({
    page,
  }) => {
    await installTauriMock(page);
    await page.goto("/");
    await expect(
      page.getByRole("navigation", { name: "Main navigation" }),
    ).toBeVisible();
    await page.keyboard.press("Meta+k");
    const field = page.getByRole("combobox");
    await expect(field).toBeVisible();
    await field.click();

    const edge = await field.evaluate((node) => {
      const style = getComputedStyle(node);
      return {
        focusVisible: node.matches(":focus-visible"),
        outlineStyle: style.outlineStyle,
        outlineWidth: style.outlineWidth,
      };
    });

    expect(edge.focusVisible).toBe(true);
    expect(edge.outlineStyle).toBe("none");
    expect(edge.outlineWidth).toBe("0px");
  });

  /* A field with a hairline of its own: the kit draws it with a utility, and
   * a rule in the base layer loses to that utility outright, which left the
   * edge grey and the field with no indicator at all. */
  test("a bordered field shows focus in its own edge", async ({ page }) => {
    const field = await meetingsSearchField(page);
    await field.click();

    const edge = await edgeOf(field);
    expect(edge.focusVisible).toBe(true);
    expect(edge.outlineStyle).toBe("none");
    expect(edge.borderColor).toBe(BRONZE);
  });

  test("an invalid field keeps its red edge while it is corrected", async ({
    page,
  }) => {
    const field = await meetingsSearchField(page);
    await field.evaluate((node) => node.setAttribute("aria-invalid", "true"));
    await field.click();

    const edge = await edgeOf(field);
    expect(edge.focusVisible).toBe(true);
    expect(edge.borderColor).toBe(RED);
  });
});
