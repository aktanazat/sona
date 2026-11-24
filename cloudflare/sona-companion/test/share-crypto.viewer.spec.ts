import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const repository = resolve(import.meta.dirname, "../../..");
const companionViewer = readFileSync(
  resolve(repository, "cloudflare/sona-companion/public/viewer.js"),
  "utf8",
);
const pagesViewer = readFileSync(
  resolve(repository, "share-site/viewer.js"),
  "utf8",
);

// The Mac's shared pages viewer (`share-site/viewer.js`) opens the same
// encrypted payloads as this one; these helpers must stay byte-identical.
const sharedFunctions = [
  "utf8",
  "base64UrlDecode",
  "base64UrlEncode",
  "u32",
  "concat",
  "record",
  "isJsonObject",
  "isJsonString",
  "shareKey",
  "decryptPayload",
];

function functionSource(source: string, name: string): string {
  const pattern = new RegExp(`^(?:async )?function ${name}\\(.*?^\\}$`, "gmsu");
  const matches = source.match(pattern) ?? [];
  expect(matches, name).toHaveLength(1);
  return matches[0] ?? "";
}

describe("share crypto shared by both viewers", () => {
  it.each(sharedFunctions)("%s is identical in both viewers", (name) => {
    expect(functionSource(companionViewer, name)).toBe(
      functionSource(pagesViewer, name),
    );
  });
});
