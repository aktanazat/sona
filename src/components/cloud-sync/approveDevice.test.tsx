import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { TooltipProvider } from "@/components/vg/tooltip";
import type { CloudPairingOffer } from "@/bindings";
import { CloudSyncApproveDevice } from "./CloudSyncPanel";
import type { CandidateApprovalState } from "./cloudSync";

/* The field an operator approves a phone from. The four states are what the
 * screen owes them: nothing pasted, a paste being read, a paste this Mac could
 * not verify, and a fingerprint to compare against the device's own screen.
 *
 * Static rendering runs no effects, so no command is reachable from here — the
 * state is passed in, which is exactly how the panel hands it over. The copy
 * comes from the shipped bundle, so a missing key fails here as a raw key. */

const english = JSON.parse(
  fs.readFileSync(
    path.join(
      path.dirname(fileURLToPath(import.meta.url)),
      "..",
      "..",
      "i18n",
      "locales",
      "en",
      "translation.json",
    ),
    "utf8",
  ),
);

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: english } },
  interpolation: { escapeValue: false },
});

const OFFER: CloudPairingOffer = {
  protocol_version: 1,
  vault_id: "vaultid123456789",
  device_id: "phonedeviceid123",
  signing_public_key: "signing-key",
  pairing_public_key: "pairing-key",
  candidate_proof: "proof",
  pairing_nonce: "nonce",
  expires_at_utc_ms: 1_800_000_000_000,
  fingerprint: "phone-shown1",
};

const paint = (state: CandidateApprovalState, offerText = ""): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <TooltipProvider>
        <CloudSyncApproveDevice
          id="approve"
          offerText={offerText}
          state={state}
          pending={false}
          onOfferTextChange={() => {}}
          onApprove={() => {}}
        />
      </TooltipProvider>
    </I18nextProvider>,
  );

/* Whether the one button on this surface can be pressed. The attribute, not the
 * word: the button's own class list carries `disabled:` variants. */
const canApprove = (markup: string): boolean => {
  const button = /<button[^>]*>/.exec(markup)?.[0] ?? "";
  expect(button).not.toBe("");
  return !button.includes('disabled=""');
};


describe("the approve-a-device field", () => {
  test("an empty field states nothing and cannot be approved", () => {
    const markup = paint({ kind: "empty" });

    expect(markup).toContain("Code from the device you are adding");
    expect(markup).not.toContain("That code is not valid");
    expect(markup).not.toContain("Check this matches");
    expect(canApprove(markup)).toBe(false);
  });

  test("a paste being read says so instead of showing a stale answer", () => {
    const markup = paint({ kind: "reading" }, '{"protocol_version":1');

    expect(markup).toContain("Checking…");
    expect(canApprove(markup)).toBe(false);
  });

  test("a paste this Mac could not verify says so and blocks approval", () => {
    const markup = paint({ kind: "invalid" }, "not an offer");

    expect(markup).toContain("That code is not valid.");
    expect(canApprove(markup)).toBe(false);
  });

  test("a verified paste shows the fingerprint it derived, to compare", () => {
    const markup = paint(
      { kind: "ready", fingerprint: "AbCdEfGhIjKl", offer: OFFER },
      JSON.stringify(OFFER),
    );

    /* The fingerprint on screen is this Mac's own reading of the pasted
     * record, never the `fingerprint` field the paste carried. */
    const outsideField = markup.replace(/<textarea\b[^>]*>[\s\S]*?<\/textarea>/g, "");
    expect(outsideField).toContain("AbCdEfGhIjKl");
    expect(outsideField).not.toContain(OFFER.fingerprint);
    expect(markup).toContain(
      "Check this matches the code on the device before you approve.",
    );
    expect(canApprove(markup)).toBe(true);
  });

  test("an approved device is stated once, with nothing left to press", () => {
    const markup = paint({ kind: "approved" });

    expect(markup).toContain("Device approved.");
    expect(canApprove(markup)).toBe(false);
  });
});
