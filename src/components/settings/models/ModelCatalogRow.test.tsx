import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import type { ModelInfo } from "@/bindings";
import {
  ModelCatalogRow,
  modelRowActions,
  type ModelRowActions,
  type ModelRowState,
} from "./ModelCatalogRow";

const localeFile = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "i18n",
  "locales",
  "en",
  "translation.json",
);

/* Inline resources initialise synchronously, so no beforeAll hook is needed
 * (the repo's bun:test shim does not declare one). */
const i18n = createInstance();
void i18n.init({
  lng: "en",
  fallbackLng: "en",
  resources: {
    en: { translation: JSON.parse(fs.readFileSync(localeFile, "utf8")) },
  },
  interpolation: { escapeValue: false },
});

const BASE: ModelInfo = {
  id: "handy-computer/whisper-medium-gguf/whisper-medium-Q5_K_M.gguf",
  name: "Whisper Medium",
  description: "Accurate multilingual transcription.",
  filename: "whisper-medium-Q5_K_M.gguf",
  source: {
    HuggingFace: {
      repo_id: "handy-computer/whisper-medium-gguf",
      revision: "main",
    },
  },
  size_mb: 1536,
  is_downloaded: false,
  is_downloading: false,
  partial_size: 0,
  is_directory: false,
  engine_type: "TranscribeCpp",
  accuracy_score: 0.9,
  speed_score: 0.5,
  supports_translation: true,
  is_recommended: true,
  supported_languages: ["en", "de", "fr"],
  supports_language_selection: true,
  is_custom: false,
  supports_streaming: true,
  supports_language_detection: true,
};

interface RenderOptions {
  model?: Partial<ModelInfo>;
  state: ModelRowState;
  inMemory?: boolean;
  percentage?: number;
  error?: string;
  showQuant?: boolean;
}

const render = (options: RenderOptions): string =>
  renderToStaticMarkup(
    <I18nextProvider i18n={i18n}>
      <ul>
        <ModelCatalogRow
          model={{ ...BASE, ...options.model }}
          state={options.state}
          inMemory={options.inMemory ?? false}
          percentage={options.percentage}
          error={options.error}
          showQuant={options.showQuant ?? false}
          onDownload={() => undefined}
          onCancel={() => undefined}
          onDelete={() => undefined}
          onActivate={() => undefined}
          onRetry={() => undefined}
        />
      </ul>
    </I18nextProvider>,
  );

describe("ModelCatalogRow status text", () => {
  test("every state reads as words, never as color alone", () => {
    const expected: [ModelRowState, string][] = [
      ["not-downloaded", "Not downloaded"],
      ["downloaded", "Downloaded"],
      ["active", "Active"],
      ["verifying", "Verifying…"],
      ["extracting", "Extracting…"],
      ["loading", "Loading…"],
    ];
    for (const [state, text] of expected) {
      expect(render({ state })).toContain(text);
    }
  });

  test("the active row distinguishes selected from resident in memory", () => {
    expect(render({ state: "active", inMemory: true })).toContain(
      "Active, in memory",
    );
    expect(render({ state: "active", inMemory: false })).toContain(">Active<");
  });

  test("a download reports its own percentage", () => {
    const html = render({ state: "downloading", percentage: 42.4 });
    expect(html).toContain("Downloading 42%");
  });
});

/* Exactly one action is inline per row; delete lives behind the row's
 * overflow, which Radix mounts only once a pointer has opened it. So the
 * inline control is asserted on the markup and the whole offer is asserted on
 * `modelRowActions`, which is what the menu renders. */
describe("modelRowActions", () => {
  const offers: [ModelRowState, ModelRowActions][] = [
    ["not-downloaded", { primary: "download", canDelete: false }],
    ["downloading", { primary: "cancel", canDelete: false }],
    ["verifying", { primary: null, canDelete: false }],
    ["extracting", { primary: null, canDelete: false }],
    ["downloaded", { primary: "activate", canDelete: true }],
    ["active", { primary: null, canDelete: true }],
    ["loading", { primary: null, canDelete: false }],
  ];

  test("every state offers exactly one inline action, or none", () => {
    for (const [state, expected] of offers) {
      expect(modelRowActions(state, false)).toEqual(expected);
    }
  });

  test("a failed transfer replaces Download with Retry rather than adding it", () => {
    expect(modelRowActions("not-downloaded", true).primary).toBe("retry");
  });

});

describe("ModelCatalogRow actions", () => {
  test("a model not on disk offers exactly one action: download", () => {
    const html = render({ state: "not-downloaded" });
    expect(html).toContain(">Download<");
    expect(html.includes(">Use<")).toBe(false);
    expect(html.includes('aria-haspopup="menu"')).toBe(false);
  });

  test("a download in flight can be cancelled and nothing else", () => {
    const html = render({ state: "downloading", percentage: 10 });
    expect(html).toContain("Cancel download");
    expect(html.includes(">Download<")).toBe(false);
    expect(html.includes('aria-haspopup="menu"')).toBe(false);
  });

  test("a downloaded model can be activated, and reaches delete", () => {
    const html = render({ state: "downloaded" });
    expect(html).toContain(">Use<");
    expect(html).toContain('aria-haspopup="menu"');
  });

  test("the active model reaches delete but cannot be re-activated", () => {
    const html = render({ state: "active" });
    expect(html).toContain('aria-haspopup="menu"');
    expect(html.includes(">Use<")).toBe(false);
  });

  test("verifying and extracting offer nothing at all", () => {
    // SAFETY: both literals are members of the closed ModelRowState union; the
    // assertion only names the array's element type for the loop.
    for (const state of ["verifying", "extracting"] as ModelRowState[]) {
      const html = render({ state });
      expect(html.includes('aria-haspopup="menu"')).toBe(false);
      expect(html.includes(">Download<")).toBe(false);
    }
  });

  test("each control names the model it acts on", () => {
    // 85 rows all showing "Use" would be indistinguishable to a screen
    // reader, so the accessible name carries the model.
    const downloaded = render({ state: "downloaded" });
    expect(downloaded).toContain('aria-label="Use Whisper Medium"');
    expect(downloaded).toContain(
      'aria-label="More actions for Whisper Medium"',
    );
    expect(render({ state: "not-downloaded" })).toContain(
      'aria-label="Download Whisper Medium"',
    );
    expect(render({ state: "not-downloaded", error: "boom" })).toContain(
      'aria-label="Retry downloading Whisper Medium"',
    );
  });
});

describe("ModelCatalogRow download progress", () => {
  test("an out-of-range percentage is clamped rather than drawn", () => {
    expect(render({ state: "downloading", percentage: 140 })).toContain(
      "width:100%",
    );
    expect(render({ state: "downloading", percentage: -5 })).toContain(
      "width:0%",
    );
  });
});

describe("ModelCatalogRow failures", () => {
  test("a hash or transfer failure stays on the row with a retry", () => {
    const html = render({
      state: "not-downloaded",
      error: "Checksum mismatch for whisper-medium-Q5_K_M.gguf",
    });
    expect(html).toContain("Checksum mismatch for whisper-medium-Q5_K_M.gguf");
    expect(html).toContain(">Retry<");
    // The failed row does not also offer a bare Download; Retry is the action.
    expect(html.includes(">Download<")).toBe(false);
  });

  test("an error on a downloaded model still allows use and delete", () => {
    const html = render({ state: "downloaded", error: "Extraction failed" });
    expect(html).toContain("Extraction failed");
    expect(html).toContain(">Use<");
    expect(html).toContain('aria-haspopup="menu"');
    // An extraction failure on a model already on disk is not a transfer
    // failure, so the row keeps its ordinary action rather than a Retry.
    expect(modelRowActions("downloaded", true).primary).toBe("activate");
  });
});

/* The row is a table row — name, size, state, one action. Everything else the
 * catalog knows rides the name's tooltip, because at the 704px content column
 * a fifth visible column can only be bought by truncating the name. These
 * assertions pin WHERE each datum lives, not just that it exists. */
describe("ModelCatalogRow metadata", () => {
  const titleOf = (html: string) => {
    const at = html.indexOf('<h3 title="');
    return html.slice(at + 11, html.indexOf('"', at + 11));
  };

  test("reports the model's download size", () => {
    const html = render({ state: "not-downloaded" });
    expect(html).toContain("1.5 GB");
  });

  test("reach and capabilities ride the name's tooltip", () => {
    const title = titleOf(render({ state: "not-downloaded" }));
    expect(title).toContain("Accurate multilingual transcription.");
    expect(title).toContain("3 languages");
    expect(title).toContain("Streaming");
    expect(title).toContain("Translate");
  });

  test("a single-language model names that language", () => {
    expect(
      titleOf(
        render({
          state: "not-downloaded",
          model: { supported_languages: ["en"] },
        }),
      ),
    ).toContain("English only");
  });

  test("Recommended stays visible; other provenance does not", () => {
    // It is the one datum that changes which row a first-run user picks, so
    // it earns a cell. Custom and Legacy describe a build already chosen.
    expect(render({ state: "not-downloaded" })).toContain(">Recommended<");
    expect(
      titleOf(
        render({
          state: "downloaded",
          model: {
            is_custom: true,
            is_recommended: false,
            source: "Local",
            filename: "my-model.gguf",
          },
        }),
      ),
    ).toContain("Custom");
    expect(
      titleOf(
        render({
          state: "downloaded",
          model: {
            is_recommended: false,
            source: {
              Url: { url: "https://blob/ggml-small.bin", sha256: null },
            },
          },
        }),
      ),
    ).toContain("Legacy");
  });

  test("the quant label appears only in debug mode", () => {
    expect(render({ state: "not-downloaded", showQuant: true })).toContain(
      "Q5_K_M",
    );
    expect(
      render({ state: "not-downloaded", showQuant: false }).includes("Q5_K_M"),
    ).toBe(false);
  });
});
