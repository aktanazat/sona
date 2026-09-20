import { z } from "zod";
import { type CardManifest, type Cluster, type ThoughtManifest, clusterHue } from "./objects";

export const PROMPT_VERSION = 1;
export const UNSORTED: Cluster = { hue: clusterHue("unsorted"), key: "unsorted", name: "Unsorted" };

export interface SorterModel {
  apiKey: string;
  baseUrl: string;
  model: string;
}

const Classification = z.object({
  cluster: z.string().trim().min(1).max(40),
  summary: z.string().trim().max(400),
  tags: z.array(z.string().trim().min(1).max(30)).max(6),
  title: z.string().trim().min(1).max(80),
});
export type Classification = z.infer<typeof Classification>;

const ChatCompletion = z.object({
  choices: z.array(z.object({ message: z.object({ content: z.string() }) })).min(1),
});

const SYSTEM_PROMPT = `You file one note into a personal brainstorm board. The notes are quick thoughts a person captured by voice or by typing, sometimes with a link. Reply with one JSON object and nothing else:
{"title": string, "summary": string, "tags": string[], "cluster": string}
- title: at most 8 words, in the note's own language, no trailing period.
- summary: one or two plain sentences that keep every concrete detail (names, numbers, decisions). Empty string when the note is already one short sentence.
- tags: up to 5 lowercase single words or hyphenated pairs.
- cluster: the board column this belongs to. Reuse one of the existing clusters when the note fits it; otherwise name a new one in 1 to 3 words, Title Case.`;

/** The cluster key: lowercase ASCII words joined by hyphens, as the board groups by it. */
export function clusterKey(name: string): string {
  const key = name
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/gu, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/gu, "-")
    .replace(/^-+|-+$/gu, "");
  return key.length === 0 ? UNSORTED.key : key;
}

function userPrompt(thought: ThoughtManifest, clusters: readonly Cluster[]): string {
  const lines = [
    `Existing clusters: ${clusters.length === 0 ? "(none yet)" : clusters.map((cluster) => cluster.name).join(", ")}`,
    `Captured by: ${thought.origin}`,
  ];
  if (thought.link !== null) lines.push(`Link: ${thought.link.url}`);
  if (thought.images.length > 0) lines.push(`Attached images: ${thought.images.length}`);
  lines.push("", "Note:", thought.text);
  return lines.join("\n");
}

/** Ask the model to title, summarize, tag and cluster one thought. */
export async function classify(
  model: SorterModel,
  thought: ThoughtManifest,
  clusters: readonly Cluster[],
): Promise<Classification> {
  const response = await fetch(`${model.baseUrl.replace(/\/$/u, "")}/chat/completions`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${model.apiKey}`,
      "content-type": "application/json",
    },
    body: JSON.stringify({
      model: model.model,
      messages: [
        { role: "system", content: SYSTEM_PROMPT },
        { role: "user", content: userPrompt(thought, clusters) },
      ],
      response_format: { type: "json_object" },
      temperature: 0.2,
    }),
  });
  if (!response.ok) {
    throw new Error(`model ${response.status}: ${(await response.text()).slice(0, 200)}`);
  }
  const completion = ChatCompletion.parse(await response.json());
  return parseClassification(completion.choices[0]?.message.content ?? "");
}

/**
 * The classification in a reply. Some models fence the object in ```json even
 * under json_object mode (gemma4 via ollama did), so the fenced body wins over
 * the raw content.
 */
export function parseClassification(content: string): Classification {
  const fenced = /```(?:json)?\s*(\{[\s\S]*\})\s*```/u.exec(content);
  return Classification.parse(JSON.parse(fenced?.[1] ?? content));
}

/** Resolve the model's cluster name against the board, so spelling drift never forks a column. */
export function resolveCluster(name: string, clusters: readonly Cluster[]): Cluster {
  const key = clusterKey(name);
  const existing = clusters.find((cluster) => cluster.key === key);
  if (existing !== undefined) return existing;
  return { hue: clusterHue(key), key, name: name.trim() };
}

export interface CardInput {
  classification: Classification | null;
  clusters: readonly Cluster[];
  model: string;
  sorterDeviceId: string;
  thought: ThoughtManifest;
  thoughtId: string;
  thoughtRevisionId: string;
  writtenAtUtcMs: number;
}

/**
 * The card for one thought. Without a classification (the thought has no text
 * to read) it lands in Unsorted with what the capture itself offers.
 */
export function buildCard(input: CardInput): CardManifest {
  const { classification, thought } = input;
  const fallbackTitle =
    thought.link !== null
      ? new URL(thought.link.url).hostname
      : thought.images.length > 0
        ? `${thought.images.length} image${thought.images.length === 1 ? "" : "s"}`
        : thought.audio !== null
          ? "Voice note"
          : "Empty note";
  return {
    archived: false,
    cluster:
      classification === null
        ? UNSORTED
        : resolveCluster(classification.cluster, input.clusters),
    format_version: 1,
    kind: "card",
    pinned: false,
    sorter: classification === null ? null : { model: input.model, prompt_version: PROMPT_VERSION },
    summary: classification?.summary ?? "",
    tags: classification?.tags ?? [],
    thought_id: input.thoughtId,
    thought_revision_id: input.thoughtRevisionId,
    title: classification?.title ?? fallbackTitle,
    writer: { device_id: input.sorterDeviceId, role: "sorter" },
    written_at_utc_ms: input.writtenAtUtcMs,
  };
}
