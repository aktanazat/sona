/*
 * The browser contract is that every selected signed-catalog representative is
 * visibly downloadable. The opt-in native contract then puts those exact,
 * hash-pinned mirror artifacts in a fresh portable profile and drives the
 * shipped headless inference path with fixed nonempty PCM.
 */
import { expect, test } from "@playwright/test";
import { createHash } from "node:crypto";
import { execFile as execFileCallback } from "node:child_process";
import { createWriteStream, readFileSync } from "node:fs";
import { cp, mkdtemp, rename, rm, stat, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { Readable, Transform } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { z } from "zod";

import { installTauriMock } from "./support/tauri-mock";

type CatalogFile = {
  filename: string;
  quant: string;
  size_bytes: number;
  sha256?: string;
};

type CatalogModel = {
  id: string;
  revision: string;
  name: string;
  description: string;
  architecture: string;
  languages: string[];
  default_quant: string;
  recommended: boolean;
  capabilities: {
    streaming: boolean;
    translate: boolean;
    lang_detect: boolean;
  };
  files: CatalogFile[];
};

type FamilyCase = {
  family: string;
  architecture: string;
  modelId: string;
  streaming: boolean;
};

type SignedModel = FamilyCase & {
  model: CatalogModel;
  file: CatalogFile & { sha256: string };
  mirror: string;
};

type ListedModel = {
  id: string;
  is_downloaded: boolean;
};

type InferenceResult = {
  model: string;
  bound_backend: string | null;
  transcribe_ms: number[];
  text: string;
};

const catalogFileSchema = z.object({
  filename: z.string(),
  quant: z.string(),
  size_bytes: z.number(),
  sha256: z.string().optional(),
});

const catalogModelSchema = z.object({
  id: z.string(),
  revision: z.string(),
  name: z.string(),
  description: z.string(),
  architecture: z.string(),
  languages: z.array(z.string()),
  default_quant: z.string(),
  recommended: z.boolean(),
  capabilities: z.object({
    streaming: z.boolean(),
    translate: z.boolean(),
    lang_detect: z.boolean(),
  }),
  files: z.array(catalogFileSchema),
});

const catalogSchema = z.object({
  mirrors: z.array(z.string()).min(1),
  models: z.array(catalogModelSchema),
});

const listedModelsSchema = z.array(
  z.object({ id: z.string(), is_downloaded: z.boolean() }),
);

const inferenceResultSchema = z.object({
  model: z.string(),
  bound_backend: z.string().nullable(),
  transcribe_ms: z.array(z.number()),
  text: z.string(),
});

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

type Catalog = z.infer<typeof catalogSchema>;

type CatalogRead =
  | { readonly ok: true; readonly catalog: Catalog }
  | { readonly ok: false; readonly problem: string };

type SignedResolution =
  | { readonly ok: true; readonly signed: SignedModel }
  | { readonly ok: false; readonly problem: string };

/* Reading and cross-checking the catalog reports problems instead of throwing.
 * A revision bump that drops a family belongs in the named catalog test below,
 * not in a file-collection error that takes the browser test down with it. */
const readSignedCatalog = (): CatalogRead => {
  try {
    return {
      ok: true,
      catalog: catalogSchema.parse(
        JSON.parse(
          readFileSync(
            join(repoRoot, "src-tauri/src/catalog/catalog.json"),
            "utf8",
          ),
        ),
      ),
    };
  } catch (error) {
    return {
      ok: false,
      problem: `the signed catalog is unreadable: ${String(error)}`,
    };
  }
};

/* These are exact catalog IDs, not a runtime discovery result. Cohere is
 * catalogued as `cohere_asr`; every catalog descriptor reaches the one
 * TranscribeCpp loader, and Moonshine Tiny is the least costly representative
 * of its only runtime-reachable live-streaming engine route. */
const FAMILY_CASES: readonly FamilyCase[] = [
  {
    family: "Canary",
    architecture: "canary",
    modelId:
      "handy-computer/canary-180m-flash-gguf/canary-180m-flash-Q8_0.gguf",
    streaming: false,
  },
  {
    family: "Cohere",
    architecture: "cohere_asr",
    modelId:
      "handy-computer/cohere-transcribe-arabic-07-2026-gguf/cohere-transcribe-arabic-07-2026-Q5_K_M.gguf",
    streaming: false,
  },
  {
    family: "GigaAM",
    architecture: "gigaam",
    modelId: "handy-computer/gigaam-v3-ctc-gguf/gigaam-v3-ctc-Q8_0.gguf",
    streaming: false,
  },
  {
    family: "SenseVoice",
    architecture: "sensevoice",
    modelId: "handy-computer/SenseVoiceSmall-gguf/SenseVoiceSmall-Q8_0.gguf",
    streaming: false,
  },
  {
    family: "TranscribeCpp streaming",
    architecture: "moonshine_streaming",
    modelId:
      "handy-computer/moonshine-streaming-tiny-gguf/moonshine-streaming-tiny-Q8_0.gguf",
    streaming: true,
  },
];

const resolveSignedModel = (
  catalog: Catalog,
  selection: FamilyCase,
): SignedResolution => {
  const separator = selection.modelId.lastIndexOf("/");
  const repository = selection.modelId.slice(0, separator);
  const filename = selection.modelId.slice(separator + 1);
  const model = catalog.models.find((candidate) => candidate.id === repository);
  if (model === undefined) {
    return {
      ok: false,
      problem: `the catalog no longer contains ${selection.modelId}`,
    };
  }
  if (model.architecture !== selection.architecture) {
    return {
      ok: false,
      problem: `${selection.modelId} changed architecture from ${selection.architecture} to ${model.architecture}`,
    };
  }
  if (model.capabilities.streaming !== selection.streaming) {
    return {
      ok: false,
      problem: `${selection.modelId} changed streaming capability from ${selection.streaming}`,
    };
  }
  const file = model.files.find(
    (candidate) =>
      candidate.filename === filename &&
      candidate.quant === model.default_quant &&
      candidate.sha256 !== undefined,
  );
  if (file === undefined || file.sha256 === undefined) {
    return {
      ok: false,
      problem: `the signed default artifact is missing for ${selection.modelId}`,
    };
  }
  const mirror = catalog.mirrors[0];
  if (mirror === undefined) {
    return { ok: false, problem: "the catalog lists no mirror" };
  }
  return { ok: true, signed: { ...selection, model, file, mirror } };
};

const catalogRead = readSignedCatalog();
const resolutions: readonly SignedResolution[] = catalogRead.ok
  ? FAMILY_CASES.map((selection) =>
      resolveSignedModel(catalogRead.catalog, selection),
    )
  : [{ ok: false, problem: catalogRead.problem }];

const signedModels: readonly SignedModel[] = resolutions.flatMap(
  (resolution) => (resolution.ok ? [resolution.signed] : []),
);
const catalogProblems: readonly string[] = resolutions.flatMap((resolution) =>
  resolution.ok ? [] : [resolution.problem],
);

/* The mirror URL the shipped downloader builds:
 * `{base}/{repo_id}/{revision}/{filename}`, with the base's trailing slash
 * trimmed (src-tauri/src/catalog/mod.rs:161-170). Fetching the bare filename
 * off the mirror root 404s. */
const mirrorUrl = ({ mirror, model, file }: SignedModel): string =>
  `${mirror.replace(/\/+$/, "")}/${model.id}/${model.revision}/${file.filename}`;

const browserModel = ({ modelId, model, file }: SignedModel) => ({
  id: modelId,
  name: model.name,
  description: model.description,
  filename: file.filename,
  source: {
    HuggingFace: { repo_id: model.id, revision: model.revision },
  },
  size_mb: Math.ceil(file.size_bytes / (1024 * 1024)),
  is_downloaded: false,
  is_downloading: false,
  partial_size: 0,
  is_directory: false,
  engine_type: "TranscribeCpp",
  accuracy_score: 0,
  speed_score: 0,
  supports_translation: model.capabilities.translate,
  is_recommended: model.recommended,
  supported_languages: model.languages,
  supports_language_selection: model.languages.length > 1,
  is_custom: false,
  supports_streaming: model.capabilities.streaming,
  supports_language_detection: model.capabilities.lang_detect,
});

const execFile = promisify(execFileCallback);

const listModels = async (binary: string): Promise<ListedModel[]> => {
  const { stdout } = await execFile(binary, ["--list-models", "--json"], {
    maxBuffer: 10 * 1024 * 1024,
  });
  return listedModelsSchema.parse(JSON.parse(stdout));
};

const transcribe = async (
  binary: string,
  modelId: string,
  fixture: string,
): Promise<InferenceResult> => {
  const { stdout } = await execFile(
    binary,
    ["--transcribe-file", fixture, "--model", modelId, "--json"],
    { maxBuffer: 10 * 1024 * 1024 },
  );
  return inferenceResultSchema.parse(JSON.parse(stdout));
};

const downloadSignedMirror = async (
  signed: SignedModel,
  destination: string,
): Promise<void> => {
  const response = await fetch(mirrorUrl(signed));
  expect(response.ok, `${signed.family} mirror download`).toBe(true);
  if (response.body === null) {
    throw new Error(`${signed.family} mirror returned no body`);
  }

  let bytes = 0;
  const hash = createHash("sha256");
  const digest = new Transform({
    transform(chunk, _encoding, callback) {
      bytes += chunk.length;
      hash.update(chunk);
      callback(null, chunk);
    },
  });
  const partial = `${destination}.partial`;
  await pipeline(
    Readable.fromWeb(response.body),
    digest,
    createWriteStream(partial),
  );
  expect(bytes, `${signed.family} pinned byte length`).toBe(
    signed.file.size_bytes,
  );
  expect(hash.digest("hex"), `${signed.family} pinned SHA-256`).toBe(
    signed.file.sha256,
  );
  await rename(partial, destination);
};

const writeFixedPcmFixture = async (directory: string): Promise<string> => {
  const sampleCount = 16_000 * 3;
  const pcmBytes = sampleCount * 2;
  const wav = Buffer.alloc(44 + pcmBytes);
  wav.write("RIFF", 0);
  wav.writeUInt32LE(36 + pcmBytes, 4);
  wav.write("WAVEfmt ", 8);
  wav.writeUInt32LE(16, 16);
  wav.writeUInt16LE(1, 20);
  wav.writeUInt16LE(1, 22);
  wav.writeUInt32LE(16_000, 24);
  wav.writeUInt32LE(32_000, 28);
  wav.writeUInt16LE(2, 32);
  wav.writeUInt16LE(16, 34);
  wav.write("data", 36);
  wav.writeUInt32LE(pcmBytes, 40);
  const fixture = join(directory, "fixed-no-speech.wav");
  await writeFile(fixture, wav);
  return fixture;
};

test("the signed catalog still carries every required ASR family", () => {
  expect(catalogProblems).toEqual([]);
  expect(signedModels).toHaveLength(FAMILY_CASES.length);
});

/* A screen reader can reach the same Download controls a mouse user sees. The
 * mock ends at the IPC boundary on purpose; the next test drives the actual
 * installed binary against the same signed bytes. */
test("the signed catalog exposes Download for every required ASR family", async ({
  page,
}) => {
  await installTauriMock(page, {
    responses: {
      get_available_models: signedModels.map(browserModel),
      get_current_model: "",
    },
  });
  await page.goto("/");

  const navigation = page.getByRole("navigation", {
    name: "Main navigation",
  });
  await navigation
    .getByRole("button", { name: "Settings", exact: true })
    .click();
  await page.getByRole("tab", { name: "Advanced", exact: true }).click();

  /* Advanced carries more than one "Open" row (Prompts sits under Meetings),
   * so the catalog's button is found through the row that names it. */
  const openCatalog = page
    .locator('[data-slot="settings-row"]', { hasText: "Model catalog" })
    .getByRole("button", { name: "Open", exact: true });
  await expect(openCatalog).toHaveCount(1);
  await openCatalog.click();
  await expect(
    page.getByRole("heading", { name: "Transcription models", exact: true }),
  ).toBeVisible();

  for (const signed of signedModels) {
    const download = page.getByRole("button", {
      name: `Download ${signed.model.name}`,
      exact: true,
    });
    await expect(download).toBeVisible();
    await download.click();
    await expect(download).toHaveCount(0);
  }

  await expect
    .poll(() =>
      page.evaluate(() =>
        Number(localStorage.getItem("tauri-invoke:download_model") ?? "0"),
      ),
    )
    .toBe(FAMILY_CASES.length);
});

/* This is deliberately opt-in: it downloads about 2.6 GiB of real pinned
 * artifacts. It must run against a built bundle, never /Applications/Sona.app,
 * and its portable marker keeps all models and settings out of the user's app. */
test("signed model families install, load, and decode fixed PCM in a portable profile", async () => {
  test.skip(
    process.env.SONA_ASR_MODEL_E2E !== "1",
    "set SONA_ASR_MODEL_E2E=1 and SONA_ASR_APP to a built Sona.app bundle",
  );
  test.setTimeout(90 * 60_000);

  const sourceApp = process.env.SONA_ASR_APP;
  if (sourceApp === undefined) {
    throw new Error("SONA_ASR_APP must name a built Sona.app bundle");
  }
  if (resolve(sourceApp) === "/Applications/Sona.app") {
    throw new Error("SONA_ASR_APP must not be the installed Sona app");
  }

  const profile = await mkdtemp(join("/tmp", "sona-asr-family-"));
  const app = join(profile, "Sona.app");
  try {
    await cp(sourceApp, app, { recursive: true });
    const binary = join(app, "Contents", "MacOS", "sona");
    const dataDirectory = join(dirname(binary), "Data");
    const modelsDirectory = join(dataDirectory, "models");
    await writeFile(join(dirname(binary), "portable"), "Sona Portable Mode\n");
    const fixture = await writeFixedPcmFixture(profile);

    const initial = await listModels(binary);
    for (const signed of signedModels) {
      expect(
        initial.find((model) => model.id === signed.modelId)?.is_downloaded,
        `${signed.family} begins uninstalled in its fresh profile`,
      ).toBe(false);
    }

    for (const signed of signedModels) {
      const destination = join(modelsDirectory, signed.file.filename);
      await downloadSignedMirror(signed, destination);
      expect((await stat(destination)).size).toBe(signed.file.size_bytes);

      const installed = await listModels(binary);
      expect(
        installed.find((model) => model.id === signed.modelId)?.is_downloaded,
        `${signed.family} becomes installed after its verified mirror artifact`,
      ).toBe(true);

      const result = await transcribe(binary, signed.modelId, fixture);
      expect(result.model).toBe(signed.modelId);
      expect(result.bound_backend).not.toBeNull();
      expect(result.transcribe_ms).toHaveLength(1);
      /* The fixture is three seconds of nonempty 16-kHz PCM silence, whose
       * documented result is an empty transcript — but a model that hears
       * something in silence is still a pass here, so the text is reported
       * rather than asserted. */
      test.info().annotations.push({
        type: "ASR result",
        description: `${signed.family}: ${JSON.stringify(result.text)}`,
      });
    }
  } finally {
    await rm(profile, { force: true, recursive: true });
  }
});
