import { expect, test, type Page } from "@playwright/test";
import type { ModelInfo } from "@/bindings";
import { APP_SETTINGS } from "./support/tauri-fixtures";
import { installTauriMock, type JsonValue } from "./support/tauri-mock";

const MULTILINGUAL: ModelInfo = {
  id: "multilingual",
  name: "Multilingual model",
  description: "",
  filename: "multilingual.gguf",
  source: { HuggingFace: { repo_id: "test/multilingual", revision: "main" } },
  size_mb: 100,
  is_downloaded: true,
  is_downloading: false,
  partial_size: 0,
  is_directory: false,
  engine_type: "TranscribeCpp",
  accuracy_score: 0.8,
  speed_score: 0.8,
  supports_translation: false,
  is_recommended: true,
  supported_languages: ["en-US", "fr-FR", "zh"],
  supports_language_selection: true,
  is_custom: false,
  supports_streaming: false,
  supports_language_detection: true,
};

interface PracticeHost {
  __TAURI_INTERNALS__: {
    invoke: (
      command: string,
      args?: Record<string, JsonValue>,
    ) => Promise<JsonValue>;
  };
}

declare global {
  interface Window {
    __TAURI_INTERNALS__: PracticeHost["__TAURI_INTERNALS__"];
  }
}

const openFirstRun = async (
  page: Page,
  options: {
    deferStop?: boolean;
    deferFinish?: boolean;
    noSpeech?: boolean;
  } = {},
) => {
  const settings = { ...APP_SETTINGS, onboarding_completed: false };
  await installTauriMock(page, {
    responses: {
      get_app_settings: settings,
      get_settings: settings,
      get_available_models: [
        {
          ...MULTILINGUAL,
          id: "english",
          name: "English only model",
          is_downloaded: false,
          supported_languages: ["en"],
        },
        {
          ...MULTILINGUAL,
          id: "manual",
          name: "Manual language model",
          is_downloaded: false,
          supports_language_detection: false,
        },
        MULTILINGUAL,
      ],
    },
  });
  await page.addInitScript((behavior) => {
    const target = window;
    const internals = target.__TAURI_INTERNALS__;
    const originalInvoke = internals.invoke;
    let activeSession: string | null = null;
    let stops = 0;
    const invoke: PracticeHost["__TAURI_INTERNALS__"]["invoke"] = async (
      command,
      args,
    ) => {
      if (command === "start_onboarding_practice") {
        activeSession = String(args?.sessionId);
        return null;
      }
      if (command === "stop_onboarding_practice") {
        const id = activeSession;
        stops += 1;
        if (behavior.noSpeech && stops === 1)
          throw "No speech heard. Try again.";
        if (behavior.deferStop) {
          const gate = Promise.withResolvers<void>();
          target.addEventListener("practice-stop", () => gate.resolve(), {
            once: true,
          });
          await gate.promise;
        }
        return {
          session_id: id,
          text: "These words came from the microphone.",
        };
      }
      if (command === "cancel_onboarding_practice") {
        activeSession = null;
        return null;
      }
      if (command === "complete_onboarding_practice") {
        if (behavior.deferFinish) {
          const gate = Promise.withResolvers<void>();
          target.addEventListener("practice-finish", () => gate.resolve(), {
            once: true,
          });
          await gate.promise;
        }
        return null;
      }
      return originalInvoke(command, args);
    };
    internals.invoke = invoke;
  }, options);
  await page.goto("/");
  await expect(
    page.getByRole("combobox", { name: /Dictation languages/ }),
  ).toBeVisible();
};

const openPractice = async (page: Page) => {
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  await page
    .getByRole("button", { name: "Multilingual model", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Try a dictation", exact: true }),
  ).toBeVisible();
};

test("automatic language choice excludes English-only and non-detecting recommendations", async ({
  page,
}) => {
  await openFirstRun(page);
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Multilingual model", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "English only model", exact: true }),
  ).toHaveCount(0);
  await expect(page.getByRole("button", { name: /Other models/ })).toHaveCount(
    0,
  );
});

for (const languages of [["French", "English"], ["Chinese (Traditional)"]]) {
  test(`chosen languages ${languages.join(" and ")} retain a base-compatible model`, async ({
    page,
  }) => {
    await openFirstRun(page);
    await page.getByRole("combobox", { name: /Dictation languages/ }).click();
    for (const language of languages) {
      await page.getByPlaceholder("Search languages").fill(language);
      await page.getByRole("option", { name: language, exact: true }).click();
    }
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "Multilingual model", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "English only model", exact: true }),
    ).toHaveCount(0);
  });
}

test("setup finishes only after delivered speech and backend confirmation", async ({
  page,
}) => {
  await openFirstRun(page, { deferFinish: true });
  await openPractice(page);
  await expect(
    page.getByRole("textbox", { name: "Your words", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Finish setup", exact: true }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Record", exact: true }).click();
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  const words = page.getByRole("textbox", { name: "Your words", exact: true });
  await expect(words).toHaveValue("These words came from the microphone.");
  await words.fill("");
  await expect(
    page.getByRole("button", { name: "Finish setup", exact: true }),
  ).toBeDisabled();
  await words.fill("An edited practice dictation.");
  await page.getByRole("button", { name: "Finish setup", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Saving setup…");
  await expect(
    page.getByRole("navigation", { name: "Main navigation" }),
  ).toHaveCount(0);
  await page.evaluate(() => {
    window.dispatchEvent(new Event("practice-finish"));
  });
  await expect(
    page.getByRole("navigation", { name: "Main navigation" }),
  ).toBeVisible();
});

test("cancelled speech cannot arrive in a later practice attempt", async ({
  page,
}) => {
  await openFirstRun(page, { deferStop: true });
  await openPractice(page);
  await page.getByRole("button", { name: "Record", exact: true }).click();
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Transcribing…");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Record", exact: true }),
  ).toBeEnabled();
  await page.evaluate(() => {
    window.dispatchEvent(new Event("practice-stop"));
  });
  await page.getByRole("button", { name: "Record", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Stop", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("textbox", { name: "Your words", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Finish setup", exact: true }),
  ).toHaveCount(0);
});

test("silence cannot be replaced with typed words and retry recovers", async ({
  page,
}) => {
  await openFirstRun(page, { noSpeech: true });
  await openPractice(page);
  await page.getByRole("button", { name: "Record", exact: true }).click();
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect(page.getByRole("alert")).toHaveText(
    "No speech heard. Record again.",
  );
  await expect(
    page.getByRole("textbox", { name: "Your words", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Finish setup", exact: true }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect(
    page.getByRole("textbox", { name: "Your words", exact: true }),
  ).toHaveValue("These words came from the microphone.");
});

test("practice stops at the visible recording limit", async ({ page }) => {
  await page.clock.install();
  await openFirstRun(page);
  await openPractice(page);
  await page.getByRole("button", { name: "Record", exact: true }).click();
  await expect(page.getByRole("status")).toContainText(
    "Listening · 30 seconds left",
  );
  await page.clock.runFor(30_000);
  await expect(
    page.getByRole("textbox", { name: "Your words", exact: true }),
  ).toHaveValue("These words came from the microphone.");
  await expect(
    page.getByRole("button", { name: "Stop", exact: true }),
  ).toHaveCount(0);
});
