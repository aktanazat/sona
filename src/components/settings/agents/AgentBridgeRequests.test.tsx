import { describe, expect, test } from "bun:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { AgentBridgeObservedRequest } from "@/bindings";
import { AgentBridgeRequests } from "./AgentBridgeRequests";

/* Which requests offer Allow and Deny is a safety question, not a styling one:
 * a button that writes a reply nobody is waiting to claim leaves the agent
 * blocked and the reader believing they answered.
 *
 * The backend already decides this per invocation and publishes it as
 * `awaiting_response`, so the row's whole job is to obey that flag and to say
 * so when it is false. These tests pin both directions and the one thing that
 * must never appear: an answer offered while the bridge is not interactive. */

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: {
      translation: {
        settings: {
          agents: {
            controls: {
              providers: {
                claude: { label: "Claude" },
                codex: { label: "Codex" },
                omp: { label: "OMP" },
              },
            },
            observed: {
              requests: "Requests",
              noRequests: "No requests yet.",
              requestKinds: {
                permission_request: "Permission request",
                pre_tool_use: "Before tool use",
              },
              expires: "Expires {{time}}",
              allowExact: "Allow exactly this request",
              denyExact: "Deny exactly this request",
              dismiss: "Dismiss",
              observeOnly:
                "Sona can only watch this request. Answer it in {{agent}}.",
            },
          },
        },
      },
    },
  },
  interpolation: { escapeValue: false },
});

const request = (
  overrides: Partial<AgentBridgeObservedRequest>,
): AgentBridgeObservedRequest => ({
  id: "invocation",
  session_id: "session",
  agent: "codex",
  kind: "permission_request",
  tool_name: "Bash",
  tool_input_preview: null,
  permission_mode: "default",
  expires_at_ms: 1_764_000_000_000,
  state: "observed",
  awaiting_response: true,
  ...overrides,
});

const paint = (
  row: AgentBridgeObservedRequest,
  interactiveReady = true,
): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <AgentBridgeRequests
        requests={[row]}
        interactiveReady={interactiveReady}
        expiryTimeFormatter={new Intl.DateTimeFormat("en", { hour: "numeric" })}
        decidePermission={async () => {}}
        dismissRequest={async () => {}}
      />
    </I18nextProvider>,
  );

const hasButton = (markup: string, label: string): boolean =>
  markup.includes(`>${label}</button>`);

describe("the observed agent requests list", () => {
  test("a waiting permission request offers both answers", () => {
    const markup = paint(request({}));
    expect(hasButton(markup, "Allow exactly this request")).toBe(true);
    expect(hasButton(markup, "Deny exactly this request")).toBe(true);
    expect(markup).not.toContain("Sona can only watch");
  });

  test("a request nobody is waiting on says so instead of offering an answer", () => {
    const markup = paint(request({ agent: "omp", awaiting_response: false }));
    expect(hasButton(markup, "Allow exactly this request")).toBe(false);
    expect(hasButton(markup, "Deny exactly this request")).toBe(false);
    expect(markup).toContain(
      "Sona can only watch this request. Answer it in OMP.",
    );
  });

  test("the answer is withheld until the bridge can actually deliver it", () => {
    const markup = paint(request({}), false);
    expect(hasButton(markup, "Allow exactly this request")).toBe(false);
    /* Not an observe-only request — the hook is waiting, this Sona just cannot
     * reach it — so the row must not claim the request is unanswerable. */
    expect(markup).not.toContain("Sona can only watch");
  });

  test("every observed request can be dismissed, whoever sent it", () => {
    for (const agent of ["claude", "codex", "omp"] as const) {
      expect(hasButton(paint(request({ agent })), "Dismiss")).toBe(true);
    }
    expect(hasButton(paint(request({ state: "responded" })), "Dismiss")).toBe(
      false,
    );
  });
});
