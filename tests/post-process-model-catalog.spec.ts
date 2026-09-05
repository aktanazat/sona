import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import { installTauriMock } from "./support/tauri-mock";
import { APP_SETTINGS, MODES_SNAPSHOT } from "./support/tauri-fixtures";

const readyCatalog = {
  provider_id: "openai",
  models: [
    { id: "gpt-4.1", provenance: "provider_reported" },
    { id: "gpt-4.1-mini", provenance: "provider_reported" },
  ],
  discovery: "ready",
  allows_manual_model_id: true,
} as const;
const catalogWithoutManualIds = {
  ...readyCatalog,
  allows_manual_model_id: false,
} as const;

const configuredSecret = {
  configured: true,
  lastErrorKind: null,
  lastVerifiedAt: null,
} as const;
const configuredSettings = {
  ...APP_SETTINGS,
  post_process_secret_states: {
    ...APP_SETTINGS.post_process_secret_states,
    openai: configuredSecret,
  },
};

const inheritedSettings = {
  ...APP_SETTINGS,
  post_process_models: {
    ...APP_SETTINGS.post_process_models,
    openai: "gpt-4.1",
  },
};

const inheritedModes = {
  ...MODES_SNAPSHOT,
  modes: MODES_SNAPSHOT.modes.map((mode) =>
    mode.id === "email"
      ? {
          ...mode,
          llm: { ...mode.llm, model_id: "stale-model", provider_id: null },
        }
      : mode,
  ),
};

const invocationCount = (page: Page, command: string): Promise<number> =>
  page.evaluate(
    (name) => Number(localStorage.getItem(`tauri-invoke:${name}`) ?? "0"),
    command,
  );

const openApp = async (page: Page) => {
  await page.goto("/");
  await expect(
    page
      .getByRole("navigation", { name: "Main navigation" })
      .getByRole("button", { name: "Capture", exact: true }),
  ).toBeVisible();
};

const openModeEditor = async (page: Page) => {
  await page.keyboard.press("Meta+k");
  await page.getByRole("option", { name: "Modes", exact: true }).click();
  await page.getByRole("button", { name: /^Email\b/ }).click();
  await expect(
    page.getByRole("button", { name: "Save changes", exact: true }),
  ).toBeVisible();
};

const cleanupProviderSummary = (page: Page) =>
  page
    .locator("details")
    .filter({ hasText: /^Cleanup provider/ })
    .locator("summary");

test.describe("post-processing model catalog", () => {
  test("loads only after the cleanup disclosure and supports keyboard selection and manual IDs", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        discover_post_process_model_catalog: readyCatalog,
        get_app_settings: configuredSettings,
        get_default_settings: configuredSettings,
        get_provider_secret_state: configuredSecret,
        get_settings: configuredSettings,
      },
    });
    await openApp(page);

    await page
      .getByRole("navigation", { name: "Main navigation" })
      .getByRole("button", { name: "Settings", exact: true })
      .click();
    await page.getByRole("tab", { name: "Advanced", exact: true }).click();
    expect(
      await invocationCount(page, "discover_post_process_model_catalog"),
    ).toBe(0);

    await cleanupProviderSummary(page).click();
    await expect
      .poll(() => invocationCount(page, "discover_post_process_model_catalog"))
      .toBe(1);

    const model = page.getByRole("combobox", { name: "Model", exact: true });
    await model.click();
    const search = page.getByRole("combobox", {
      name: "Search models",
      exact: true,
    });
    await expect(search).toBeFocused();
    await search.fill("gpt-4.1-mini");
    await expect(
      page.getByRole("option", { name: /gpt-4\.1-mini.*Provider/ }),
    ).toBeVisible();
    await page.keyboard.press("Enter");
    await expect
      .poll(() => invocationCount(page, "change_post_process_model_setting"))
      .toBe(1);

    await model.click();
    await search.fill("gpt-experimental");
    await expect(
      page.getByRole("option", { name: /Use.*gpt-experimental.*Manual/ }),
    ).toBeVisible();
    await page.keyboard.press("Enter");
    await expect
      .poll(() => invocationCount(page, "change_post_process_model_setting"))
      .toBe(2);
  });

  test("does not offer manual IDs when the typed catalog forbids them", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        discover_post_process_model_catalog: catalogWithoutManualIds,
        get_app_settings: configuredSettings,
        get_default_settings: configuredSettings,
        get_provider_secret_state: configuredSecret,
        get_settings: configuredSettings,
      },
    });
    await openApp(page);

    await page
      .getByRole("navigation", { name: "Main navigation" })
      .getByRole("button", { name: "Settings", exact: true })
      .click();
    await page.getByRole("tab", { name: "Advanced", exact: true }).click();
    await cleanupProviderSummary(page).click();
    await expect
      .poll(() => invocationCount(page, "discover_post_process_model_catalog"))
      .toBe(1);

    await page.getByRole("combobox", { name: "Model", exact: true }).click();
    await page
      .getByRole("combobox", { name: "Search models", exact: true })
      .fill("gpt-experimental");
    await expect(
      page.getByRole("option", { name: /Use.*gpt-experimental/ }),
    ).toHaveCount(0);
  });

  test("offers a manual model id while the first catalog is still in flight", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        get_app_settings: configuredSettings,
        get_default_settings: configuredSettings,
        get_provider_secret_state: configuredSecret,
        get_settings: configuredSettings,
      },
      /* Discovery never answers, so the field stays in the state a reader
       * meets first: no catalog, and a model id that has to be typed. */
      pending: ["discover_post_process_model_catalog"],
    });
    await openApp(page);

    await page
      .getByRole("navigation", { name: "Main navigation" })
      .getByRole("button", { name: "Settings", exact: true })
      .click();
    await page.getByRole("tab", { name: "Advanced", exact: true }).click();
    await cleanupProviderSummary(page).click();

    await page.getByRole("combobox", { name: "Model", exact: true }).click();
    await page
      .getByRole("combobox", { name: "Search models", exact: true })
      .fill("gpt-experimental");
    await page
      .getByRole("option", { name: /Use.*gpt-experimental.*Manual/ })
      .click();
    await expect
      .poll(() => invocationCount(page, "change_post_process_model_setting"))
      .toBe(1);
  });

  test("shows an inherited mode's global model without discovery", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        get_app_settings: inheritedSettings,
        get_settings: inheritedSettings,
        get_default_settings: inheritedSettings,
        get_modes: inheritedModes,
      },
    });
    await openApp(page);
    await openModeEditor(page);

    await page.locator("summary").filter({ hasText: "Advanced" }).click();
    const model = page.getByLabel("AI model", { exact: true });
    await expect(model).toBeDisabled();
    await expect(model).toHaveValue("gpt-4.1");
    expect(
      await invocationCount(page, "discover_post_process_model_catalog"),
    ).toBe(0);
  });

  test("defers an explicit mode's discovery until its own model combobox opens", async ({
    page,
  }) => {
    await installTauriMock(page, {
      responses: {
        discover_post_process_model_catalog: readyCatalog,
        get_provider_secret_state: configuredSecret,
      },
    });
    await openApp(page);
    await openModeEditor(page);

    await page.locator("summary").filter({ hasText: "Advanced" }).click();
    const model = page.getByRole("combobox", {
      name: "AI model",
      exact: true,
    });
    await expect(model).toBeVisible();
    expect(
      await invocationCount(page, "discover_post_process_model_catalog"),
    ).toBe(0);

    await model.click();
    await expect
      .poll(() => invocationCount(page, "discover_post_process_model_catalog"))
      .toBe(1);
  });
});
