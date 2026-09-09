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
    // SAFETY: installTauriMock plants this runtime before this wrapper runs.
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

test("keeps typed local endpoint fields after a refused write", async ({
  page,
}) => {
  const settings = {
    ...APP_SETTINGS,
    meeting_local_engine: { kind: "apple_intelligence" },
    meeting_remote_intelligence_enabled: true,
  };

  await installTauriMock(page, {
    responses: {
      get_app_settings: settings,
      get_settings: settings,
      get_default_settings: settings,
    },
  });
  await page.addInitScript(() => {
    type TestWindow = Window & {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args?: Record<string, JsonValue>,
        ) => Promise<JsonValue>;
      };
    };

    // SAFETY: installTauriMock planted this runtime before this wrapper runs.
    const target = window as TestWindow;
    const originalInvoke = target.__TAURI_INTERNALS__.invoke;
    target.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "change_meeting_local_engine_setting") {
        // SAFETY: the fixture initializes this storage value as a JSON array.
        const writes = JSON.parse(
          localStorage.getItem("meeting-local-engine-writes") ?? "[]",
        ) as JsonValue[];
        writes.push(args?.engine ?? null);
        localStorage.setItem(
          "meeting-local-engine-writes",
          JSON.stringify(writes),
        );
        throw new Error("invalid endpoint");
      }
      return originalInvoke(command, args);
    };
  });

  await page.goto("/");
  await page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("button", { name: "Settings", exact: true })
    .click();
  await expect(page.getByTestId("settings-hub")).toBeVisible();
  await page.getByRole("tab", { name: "Advanced", exact: true }).click();

  const picker = page.getByRole("combobox", { name: LOCAL_ENGINE.label });
  await picker.click();
  await page
    .getByRole("option", { name: LOCAL_ENGINE.endpoint, exact: true })
    .click();
  await expect(
    page.getByRole("textbox", { name: LOCAL_ENGINE.baseUrl.label }),
  ).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(() => localStorage.getItem("meeting-local-engine-writes")),
    )
    .toBeNull();

  const endpoint = page.getByRole("textbox", {
    name: LOCAL_ENGINE.baseUrl.label,
  });
  const model = page.getByRole("textbox", { name: LOCAL_ENGINE.model.label });
  const contextWindow = page.getByRole("spinbutton", {
    name: LOCAL_ENGINE.contextWindow.label,
  });
  await model.fill(" fixture-model ");
  await contextWindow.fill("4096");
  await endpoint.fill(" http://127.0.0.1:11435/v1 ");
  await endpoint.blur();

  await expect
    .poll(async () =>
      page.evaluate(
        () =>
          JSON.parse(
            localStorage.getItem("meeting-local-engine-writes") ?? "[]",
          ).length,
      ),
    )
    .toBe(1);
  await expect(endpoint).toHaveValue(" http://127.0.0.1:11435/v1 ");
  await expect(model).toHaveValue(" fixture-model ");
  await expect(contextWindow).toHaveValue("4096");
  await expect(picker).toContainText(LOCAL_ENGINE.endpoint);

  const writes = await page.evaluate(() =>
    JSON.parse(localStorage.getItem("meeting-local-engine-writes") ?? "[]"),
  );
  expect(writes).toEqual([
    {
      kind: "local_endpoint",
      base_url: "http://127.0.0.1:11435/v1",
      model: "fixture-model",
      context_window_tokens: 4096,
    },
  ]);
});

test("renders every local endpoint status reason", async ({ page }) => {
  const settings = {
    ...APP_SETTINGS,
    meeting_local_engine: {
      kind: "local_endpoint",
      base_url: "http://127.0.0.1:11434/v1",
      model: "fixture-model",
      context_window_tokens: null,
    },
    meeting_remote_intelligence_enabled: true,
  };

  await installTauriMock(page, {
    responses: {
      get_app_settings: settings,
      get_settings: settings,
      get_default_settings: settings,
    },
  });
  await page.addInitScript(() => {
    type TestWindow = Window & {
      __TAURI_INTERNALS__: {
        invoke: (
          command: string,
          args?: Record<string, JsonValue>,
        ) => Promise<JsonValue>;
      };
      sonaSetMeetingLocalStatusIndex: (index: number) => void;
    };
    const statuses: JsonValue[] = [
      {
        kind: "local_endpoint",
        reachable: true,
        model_count: 1,
        error: "context_window_not_configured",
      },
      {
        kind: "local_endpoint",
        reachable: false,
        model_count: 0,
        error: "invalid_endpoint",
      },
      {
        kind: "local_endpoint",
        reachable: true,
        model_count: 0,
        error: "invalid_response",
      },
      {
        kind: "local_endpoint",
        reachable: false,
        model_count: 0,
        error: "unreachable",
      },
      {
        kind: "local_endpoint",
        reachable: false,
        model_count: 0,
        error: "future_status_code",
      },
    ];
    let statusIndex = 0;
    // SAFETY: installTauriMock plants this runtime before this wrapper runs.
    const target = window as TestWindow;
    const originalInvoke = target.__TAURI_INTERNALS__.invoke;
    target.sonaSetMeetingLocalStatusIndex = (index) => {
      statusIndex = index;
    };
    target.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === "meeting_local_engine_status") {
        return statuses[statusIndex] ?? statuses[0];
      }
      return originalInvoke(command, args);
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
  const expected = [
    LOCAL_ENGINE.status.endpointContextUnknown,
    LOCAL_ENGINE.status.endpointInvalid,
    LOCAL_ENGINE.status.endpointInvalidResponse,
    LOCAL_ENGINE.status.endpointUnreachable,
    LOCAL_ENGINE.status.endpointUnknown,
  ];
  await expect(
    page.getByRole("status").filter({ hasText: expected[0] }),
  ).toBeVisible();

  for (let index = 1; index < expected.length; index += 1) {
    await page.evaluate((nextIndex) => {
      // SAFETY: the init script above planted this setter.
      const target = window as Window & {
        sonaSetMeetingLocalStatusIndex: (value: number) => void;
      };
      target.sonaSetMeetingLocalStatusIndex(nextIndex);
    }, index);
    await endpoint.fill("http://127.0.0.1:" + (11434 + index) + "/v1");
    await endpoint.blur();
    await expect(
      page.getByRole("status").filter({ hasText: expected[index] }),
    ).toBeVisible();
  }
});
