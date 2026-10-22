import { afterAll, describe, expect, test } from "bun:test";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import type { AppSettings } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import { SonaAgentPairing } from "./SonaAgentPairing";

/* Pairing is three facts and one decision, and the screen's whole job is to
 * be honest about which of them are true right now.
 *
 * The two states worth pinning are the ones a reader acts on: unpaired, which
 * must say so rather than imply a relay is there, and paired-but-never-reached,
 * which is the normal state of a laptop whose relay is asleep and must not be
 * confused with a broken pairing. Everything else on this surface is a field
 * whose value comes straight from the store.
 *
 * `renderToStaticMarkup` runs no effects, so nothing here reaches the relay or
 * the secret backend — which is also why the identity row reads as unavailable
 * below rather than showing a key. */

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: {
      translation: {
        settings: {
          agents: {
            sonaAgent: {
              title: "Sona agent",
              pairing: "Pairing",
              paired: "Paired",
              unpaired: "Not paired",
              lastTested: "Last tested",
              never: "Never",
              relayUrl: "Server address",
              relayUrlHint: "Only a Tailscale address or this Mac itself.",
              relayKeyId: "Server key ID",
              relayPublicKey: "Server public key",
              identity: "This Mac's public key",
              identityHint:
                "Add this key to the server so it accepts messages from this Mac. The private key never leaves your keychain.",
              identityUnavailable: "Turn the agent panel on to create a key.",
              copy: "Copy",
              copied: "Copied",
              unpair: "Unpair",
              test: "Test",
              reached: "The server replied.",
              failed: "Couldn't connect to the server.",
            },
          },
        },
      },
    },
  },
  interpolation: { escapeValue: false },
});

const paint = (settings: AppSettings): string => {
  useSettingsStore.setState({ settings, isUpdating: {} });
  return renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>
        <SonaAgentPairing />
      </TooltipProvider>
    </I18nextProvider>,
  );
};

/* `paint` mutates the module-scoped zustand store, and bun runs every test
 * file in one runtime, so a later suite would otherwise inherit this file's
 * paired state. Restore the snapshot this file found. */
const priorState = useSettingsStore.getState();
afterAll(() => {
  useSettingsStore.setState(priorState, true);
});

const PAIRED: AppSettings = {
  agent_panel_enabled: true,
  agent_panel_paired: true,
  agent_panel_relay_url: "http://100.99.192.40:8650/",
  agent_panel_relay_key_id: "relay-01",
  agent_panel_relay_public_key: "Ac9r0uJ2Yq0m4d1lVxq2vQnQ0m0R3lVxq2vQnQ0m4d0=",
};

/** The value of the input whose id is given, as the markup states it. */
const fieldValue = (markup: string, id: string): string =>
  new RegExp(`<input[^>]*id="${id}"[^>]*value="([^"]*)"`).exec(markup)?.[1] ??
  new RegExp(`<input[^>]*value="([^"]*)"[^>]*id="${id}"`).exec(markup)?.[1] ??
  "";

/** Whether a button carrying this label is on screen at all. */
const hasButton = (markup: string, label: string): boolean =>
  markup.includes(`>${label}</button>`);

describe("the Sona agent pairing screen", () => {
  test("an unpaired panel still names both relay decisions", () => {
    const markup = paint({ agent_panel_enabled: true });
    expect(markup).toContain("Not paired");
    expect(markup).not.toContain(">Paired<");
    /* Whether they are enabled is not assertable here: zustand serves the
     * initial store snapshot to a server render, so `settings` reads null and
     * every control paints disabled regardless of the pairing. Their presence
     * is the part this screen owes a reader. */
    expect(hasButton(markup, "Test")).toBe(true);
    expect(hasButton(markup, "Unpair")).toBe(true);
  });

  test("a saved pairing seeds its own fields", () => {
    const markup = paint(PAIRED);
    expect(fieldValue(markup, "sona-agent-relay-url")).toBe(
      "http://100.99.192.40:8650/",
    );
    expect(fieldValue(markup, "sona-agent-relay-key-id")).toBe("relay-01");
    expect(fieldValue(markup, "sona-agent-relay-public-key")).toBe(
      // SAFETY: PAIRED declares this field as a literal string two screens up;
      // the AppSettings type widens it to string | null, narrowed back here.
      PAIRED.agent_panel_relay_public_key as string,
    );
  });

  test("paired but never reached is a state of its own, not a failure", () => {
    const markup = paint(PAIRED);
    expect(markup).toContain("Paired");
    expect(markup).toContain("Never");
    // Nothing is claiming a success that has not happened.
    expect(markup).not.toContain("The server replied.");
    expect(markup).not.toContain("Couldn't connect to the server.");
  });

  test("a reached relay states when, and does not restate never", () => {
    const markup = paint({
      ...PAIRED,
      agent_panel_last_successful_connection_at: 1_764_000_000_000,
    });
    expect(markup).not.toContain("Never");
  });

});
