import { expect, test } from "@playwright/test";

import en from "../src/i18n/locales/en/translation.json" with { type: "json" };
import { APP_SETTINGS } from "./support/tauri-fixtures";
import { installTauriMock, type JsonValue } from "./support/tauri-mock";

const LOCAL_ENGINE = en.settings.meetings.localEngine;
const REMOTE_INTELLIGENCE = en.settings.meetings.remoteIntelligence;
const REACHABLE_STATUS = LOCAL_ENGINE.status.endpointReachable.replace(
  "{{count}}",
  "1",
);
const STALE_STATUS = LOCAL_ENGINE.status.endpointUnreachable.replace(
  "{{error}}",
  "stale-status",
);

test("keeps a stale local engine status response from replacing the newest state", async ({
  page,
}) => {
  const settings = {
    ...APP_SETTINGS,
    meeting_local_engine: {
      kind: "local_endpoint",
      base_url: "http://127.0.0.1:11434/v1",
      model: "gemma4:12b-mlx",
      context_window_tokens: null,
    },
    meeting_remote_intelligence_enabled: true,
  };

  await installTauriMock(page, {
    pending: ["meeting_series_remote_roster"],
    responses: {
      get_app_settings: settings,
      get_settings: settings,
      get_default_settings: settings,
    },
  });
  await page.addInitScript(() => {
    type LocalStatus = {
      kind: "local_endpoint";
      reachable: boolean;
      model_count: number;
      error: string | null;
    };
    type StatusResolver = (status: LocalStatus, newest: boolean) => void;
    type TestWindow = Window & {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args?: Record<string, JsonValue>,
        ) => Promise<JsonValue>;
      };
      sonaResolveMeetingLocalEngineStatus: StatusResolver;
    };

    // SAFETY: installTauriMock planted these globals before this page ran;
    // Window does not declare them, so the intersection assertion is the way
    // to the invoke this test has to sit in front of.
    const target = window as TestWindow;
    const originalInvoke = target.__TAURI_INTERNALS__.invoke;
    const pending: Array<(status: LocalStatus) => void> = [];

    target.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command !== "meeting_local_engine_status") {
        return originalInvoke(command, args);
      }
      localStorage.setItem(
        "meeting-local-engine-status-calls",
        String(pending.length + 1),
      );
      return new Promise<LocalStatus>((resolve) => pending.push(resolve));
    };

    target.sonaResolveMeetingLocalEngineStatus = (status, newest) => {
      const resolve = newest ? pending.pop() : pending.shift();
      resolve?.(status);
    };
  });

  await page.goto("/");
  await page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("button", { name: "Settings", exact: true })
    .click();
  await expect(page.getByTestId("settings-hub")).toBeVisible();
  await page.getByRole("tab", { name: "Advanced", exact: true }).click();

  const endpoint = page.getByRole("textbox", {
    name: LOCAL_ENGINE.baseUrl.label,
  });
  await expect(endpoint).toBeVisible();
  await expect(
    page.getByText(REMOTE_INTELLIGENCE.consent, { exact: true }),
  ).not.toHaveAttribute("role", "status");
  await expect(
    page.getByText(REMOTE_INTELLIGENCE.unpaired, { exact: true }),
  ).not.toHaveAttribute("role", "status");
  await expect(
    page.getByText(REMOTE_INTELLIGENCE.loading, { exact: true }),
  ).not.toHaveAttribute("role", "status");

  const initialStatusCalls = await page.evaluate(() =>
    Number(localStorage.getItem("meeting-local-engine-status-calls") ?? "0"),
  );
  expect(initialStatusCalls).toBeGreaterThan(0);

  await endpoint.fill("http://127.0.0.1:11435/v1");
  await endpoint.blur();
  await expect
    .poll(() =>
      page.evaluate(() =>
        Number(
          localStorage.getItem("meeting-local-engine-status-calls") ?? "0",
        ),
      ),
    )
    .toBeGreaterThan(initialStatusCalls);

  const newestStatus = {
    kind: "local_endpoint" as const,
    reachable: true,
    model_count: 1,
    error: null,
  };
  const staleStatus = {
    kind: "local_endpoint" as const,
    reachable: false,
    model_count: 0,
    error: "stale-status",
  };
  await page.evaluate((status) => {
    // SAFETY: the init script above planted this resolver; Window does not
    // declare it, so the intersection assertion is the way to it.
    const target = window as Window & {
      sonaResolveMeetingLocalEngineStatus: (
        value: typeof status,
        newest: boolean,
      ) => void;
    };
    target.sonaResolveMeetingLocalEngineStatus(status, true);
  }, newestStatus);
  await page.evaluate((status) => {
    // SAFETY: as above.
    const target = window as Window & {
      sonaResolveMeetingLocalEngineStatus: (
        value: typeof status,
        newest: boolean,
      ) => void;
    };
    target.sonaResolveMeetingLocalEngineStatus(status, false);
  }, staleStatus);

  await expect(
    page.getByRole("status").filter({ hasText: REACHABLE_STATUS }),
  ).toBeVisible();
  await expect(
    page.getByRole("status").filter({ hasText: STALE_STATUS }),
  ).toHaveCount(0);
});
