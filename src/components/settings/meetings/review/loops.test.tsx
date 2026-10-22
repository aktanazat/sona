import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type {
  MeetingLoopRow,
  MeetingLoopStatus,
  PersonListEntry,
} from "@/bindings";
import { LoopRows } from "./LoopRows";

const localePath = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);
const catalogue = JSON.parse(fs.readFileSync(localePath, "utf8"));
const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: { en: { translation: catalogue } },
  interpolation: { escapeValue: false },
  parseMissingKeyHandler: () => "__MISSING__",
});
const render = (node: React.ReactElement) =>
  renderToStaticMarkup(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);
const occurrences = (markup: string, needle: string) =>
  markup.split(needle).length - 1;
const noop = () => undefined;

const PEOPLE: PersonListEntry[] = [
  {
    person: {
      id: "person-dana",
      display_name: "Dana Reyes",
      aliases: [],
      calendar_emails: [],
      organization: null,
      summary: null,
      created_at_utc_ms: 1,
      updated_at_utc_ms: 1,
    },
    meetings_count: 2,
    last_meeting_at_utc_ms: 2,
    suggested_count: 0,
    evidence_sources: ["calendar"],
    confirmed_count: 2,
    last_meeting: null,
  },
];

const row = (
  status: MeetingLoopStatus,
  overrides: Partial<MeetingLoopRow> = {},
): MeetingLoopRow => ({
  loop_id: `session-1:commitment:${status}`,
  session_id: "session-1",
  kind: "commitment",
  text: `Send the ${status} comparison`,
  owner_text: "Dana Reyes",
  owner_person_id: null,
  owner_display_name: null,
  direction: "waiting_on",
  status,
  resolved_at_utc_ms: null,
  resolving_operation_id: null,
  resolved_by: null,
  carried_into_loop_id: null,
  carried_since_at_utc_ms: null,
  at_ms: 30_000,
  revision: 0,
  instead: null,
  firmness: "firm",
  quote: "i'll send the comparison",
  speaker: "Dana",
  citations: [],
  ...overrides,
});

const rows = (loops: MeetingLoopRow[]) =>
  render(
    <LoopRows
      rows={loops}
      people={PEOPLE}
      disabled={false}
      emptyText="Nothing left open."
      onChange={noop}
      onJumpToSegment={noop}
    />,
  );

describe("actionable loop rows", () => {
  test("every row carries its status word and its receipt", () => {
    const markup = rows([row("open"), row("done"), row("dropped")]);

    expect(markup).toContain("Open");
    expect(markup).toContain("Done");
    expect(markup).toContain("Dropped");
    /* The quote is what makes closing a loop checkable, so it renders beside
     * every row rather than only on the ones still open. */
    expect(occurrences(markup, "i&#x27;ll send the comparison")).toBe(3);
  });

  test("a done row has its completion box ticked", () => {
    const markup = rows([row("done")]);

    expect(markup).toContain('aria-checked="true"');
  });

  test("an open row offers drop, a closed row offers reopen", () => {
    expect(rows([row("open")])).toContain("Drop");
    expect(rows([row("open")])).not.toContain("Reopen");

    const dropped = rows([row("dropped")]);
    expect(dropped).toContain("Reopen");
    expect(dropped).not.toContain(">Drop<");

    const carried = rows([row("carried")]);
    expect(carried).toContain("Reopen");
    expect(carried).toContain("Carried forward");
  });

  test("an assigned owner is named by the person, not the transcript", () => {
    const markup = rows([
      row("open", {
        owner_text: "dana",
        owner_person_id: "person-dana",
        owner_display_name: "Dana Reyes",
      }),
    ]);

    expect(markup).toContain("Dana Reyes");
  });
});
