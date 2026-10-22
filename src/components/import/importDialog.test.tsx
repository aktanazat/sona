import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { AudioImportJob, AudioImportStatus } from "@/bindings";
import { MEDIA_IMPORT_EXTENSIONS } from "@/hooks/useAudioImport";
import { MEETING_IMPORT_EXTENSIONS } from "@/hooks/useMeetingImport";
import { HistoryAudioImportSection } from "../settings/history/HistoryAudioImportSection";
import {
  addImportPaths,
  audioJobRowState,
  importFileExtension,
  readyImportPaths,
  setImportRow,
  type ImportRow,
} from "./importQueue";

/* The import dialog's rules, and the one list that outlives it.
 *
 * Two of the three ways a file reaches the dialog cannot be driven from a
 * test at all: the native picker is an OS panel, and an OS file drop arrives
 * as a webview event that a browser has no source for. What both of them do
 * once a path exists is `importQueue`, which is why it is a module of plain
 * functions rather than four `setState` calls inside a component. Everything
 * a drop could get wrong — a file the decoder refuses, the same file twice,
 * a row that never leaves the spinner — is decided here.
 *
 * `HistoryAudioImportSection` is in this file because it is the other half of
 * one contract: the dialog and Library draw the same jobs, and the dialog can
 * be closed while they run, so what Library says about a job is the only
 * account of it left. Radix keeps dialog content in a portal that a static
 * render never mounts, so the dialog's own markup is proved by the Playwright
 * captures in the slice report, not here.
 */

const localeFile = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localeFile, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

const render = (node: React.ReactElement): string =>
  renderToStaticMarkup(<I18nextProvider i18n={i18n}>{node}</I18nextProvider>);

const ROWS_PATHS = ["/Recordings/standup.m4a", "/Recordings/review.wav"];

const ROWS: ImportRow[] = [
  {
    path: "/Recordings/standup.m4a",
    name: "standup.m4a",
    state: "ready",
    jobId: null,
    failure: null,
  },
];

const job = (
  id: number,
  status: AudioImportStatus,
  result: AudioImportJob["result"] = null,
): AudioImportJob => ({
  id,
  file_name: `job-${id}.m4a`,
  status,
  decoded_samples: 0,
  cancel_requested: false,
  result,
});

describe("the chosen files", () => {
  test("a file the importer cannot read never becomes a row", () => {
    const rows = addImportPaths(
      [],
      ["/Recordings/notes.pdf", "/Recordings/standup.m4a"],
      MEDIA_IMPORT_EXTENSIONS,
    );

    expect(rows.map((row) => row.name)).toEqual(["standup.m4a"]);
  });

  test("a meeting takes the transcript exports a dictation refuses", () => {
    const paths = ["/Meetings/kickoff.srt", "/Meetings/kickoff.m4a"];

    expect(
      addImportPaths([], paths, MEDIA_IMPORT_EXTENSIONS).map((row) => row.name),
    ).toEqual(["kickoff.m4a"]);
    expect(
      addImportPaths([], paths, MEETING_IMPORT_EXTENSIONS).map(
        (row) => row.name,
      ),
    ).toEqual(["kickoff.srt", "kickoff.m4a"]);
  });

  test("the same file arriving twice is one row", () => {
    const once = addImportPaths([], ROWS_PATHS, MEDIA_IMPORT_EXTENSIONS);
    const twice = addImportPaths(once, ROWS_PATHS, MEDIA_IMPORT_EXTENSIONS);

    expect(twice.map((row) => row.path)).toEqual(ROWS_PATHS);
    /* Same array, not an equal one: a drag that lands on files already listed
     * must not re-render the dialog under the pointer. */
    expect(twice).toBe(once);
  });

  test("a name with no extension is not read as one", () => {
    expect(importFileExtension("/Recordings/.m4a")).toBe("");
    expect(importFileExtension("/Recordings/interview")).toBe("");
    expect(importFileExtension("/Recordings/interview.take.2.WAV")).toBe("wav");
  });

  test("pressing import twice cannot hand the same file over twice", () => {
    const queued = setImportRow(ROWS, "/Recordings/standup.m4a", {
      state: "queued",
      jobId: 7,
    });

    expect(readyImportPaths(ROWS)).toEqual(["/Recordings/standup.m4a"]);
    expect(readyImportPaths(queued)).toEqual([]);
  });
});

describe("what a job says about its row", () => {
  test("a failed job names the sentence that explains it", () => {
    expect(
      audioJobRowState(
        job(1, "failed", {
          kind: "failed",
          code: "no_audio",
          message: "no audio track",
        }),
      ),
    ).toEqual({
      state: "failed",
      failure: "settings.history.audioImport.failure.no_audio",
    });
  });

  test("a cancelled job is not a failure", () => {
    expect(
      audioJobRowState(job(2, "cancelled", { kind: "cancelled" })),
    ).toEqual({ state: "cancelled", failure: null });
  });

  test("a queued job has not started", () => {
    expect(audioJobRowState(job(3, "queued")).state).toBe("queued");
  });

  /* Two backend stages, one thing a person is waiting for. The row spins for
   * both, which is why the spinner cannot be keyed off a single status. */
  test("decoding and transcribing are both the row spinning", () => {
    expect(audioJobRowState(job(4, "decoding")).state).toBe("running");
    expect(audioJobRowState(job(5, "transcribing")).state).toBe("running");
  });

  test("a finished job stops", () => {
    expect(audioJobRowState(job(6, "done")).state).toBe("done");
  });

  test("a job the row is not following leaves the row alone", () => {
    const queued = setImportRow(ROWS, "/Recordings/standup.m4a", {
      state: "queued",
      jobId: 7,
    });

    expect(
      setImportRow(queued, "/Recordings/other.m4a", { state: "done" }),
    ).toBe(queued);
  });
});


describe("the import list Library keeps after the dialog closes", () => {
  test("a failed import still says why", () => {
    const markup = render(
      <HistoryAudioImportSection
        jobs={[
          job(1, "failed", {
            kind: "failed",
            code: "unsupported_format",
            message: "bad container",
          }),
        ]}
        error={null}
        onCancel={() => undefined}
      />,
    );

    expect(markup).toContain("This audio format is not supported.");
  });

  test("a running import offers the only way to stop it", () => {
    const markup = render(
      <HistoryAudioImportSection
        jobs={[job(2, "transcribing")]}
        error={null}
        onCancel={() => undefined}
      />,
    );

    expect(markup).toContain('data-testid="history-import-cancel"');
    expect(markup).toContain("Transcribing");
  });

  test("a finished import offers nothing to cancel", () => {
    const markup = render(
      <HistoryAudioImportSection
        jobs={[job(3, "done", { kind: "done", history_id: 12 })]}
        error={null}
        onCancel={() => undefined}
      />,
    );

    expect(markup).not.toContain('data-testid="history-import-cancel"');
  });
});
