// The board groups cards by cluster key, so a model that spells an existing
// cluster differently must land in that cluster, not open a second column.
import { describe, expect, test } from "bun:test";
import type { Cluster } from "../src/objects";
import { parseClassification, resolveCluster } from "../src/sort";

const SIDE_PROJECTS: Cluster = { hue: 212, key: "side-projects", name: "Side Projects" };

describe("resolveCluster against a board with Side Projects", () => {
  test.each([
    ["side projects"],
    ["Side-Projects"],
    ["  SIDE PROJECTS  "],
    ["Sidé Projects"],
  ])("maps %j onto the existing cluster", (spelling) => {
    expect(resolveCluster(spelling, [SIDE_PROJECTS])).toStrictEqual(SIDE_PROJECTS);
  });

  test("opens a new cluster for a name the board lacks, keyed by its slug", () => {
    expect(resolveCluster(" Reading List ", [SIDE_PROJECTS])).toStrictEqual({
      hue: expect.any(Number),
      key: "reading-list",
      name: "Reading List",
    });
  });
});

// gemma4:12b-mlx through ollama 0.32.15 answered json_object mode with this
// exact text on 2026-09-20; the sorter must file it, not retry forever.
const FENCED_REPLY =
  'json\n{```json\n{\n  "title": "Pinch to Zoom for Cluster Map",\n  "summary": "Zoom the board out to a map of clusters.",\n  "tags": ["Navigation", "Zoom"],\n  "cluster": "Navigation"\n}\n```';

describe("parseClassification", () => {
  test("reads the object out of a fenced reply", () => {
    expect(parseClassification(FENCED_REPLY)).toStrictEqual({
      cluster: "Navigation",
      summary: "Zoom the board out to a map of clusters.",
      tags: ["Navigation", "Zoom"],
      title: "Pinch to Zoom for Cluster Map",
    });
  });

  test("reads a bare object unchanged", () => {
    expect(
      parseClassification('{"title":"A","summary":"","tags":[],"cluster":"Ideas"}'),
    ).toStrictEqual({ cluster: "Ideas", summary: "", tags: [], title: "A" });
  });

  test("rejects a reply with no object", () => {
    expect(() => parseClassification("I cannot classify this.")).toThrow();
  });
});
