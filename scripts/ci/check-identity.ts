import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { z } from "zod";

const allowlistSchema = z.object({
  ignoredPaths: z.array(z.string()),
  allowedPaths: z.array(z.string()),
  allowedLinePatterns: z.array(z.string()),
});

const root = resolve(import.meta.dir, "../..");
const allowlist = allowlistSchema.parse(
  JSON.parse(
    readFileSync(resolve(import.meta.dir, "identity-allowlist.json"), "utf8"),
  ),
);
const ignoredDirectories = [
  ".git/",
  "node_modules/",
  "src-tauri/binaries/",
  "src-tauri/target/",
];
const allowedPatterns = allowlist.allowedLinePatterns.map(
  (pattern) => new RegExp(pattern, "i"),
);
const tracked = Bun.spawnSync(
  ["git", "ls-files", "-co", "--exclude-standard"],
  {
    cwd: root,
    stdout: "pipe",
  },
);

if (tracked.exitCode !== 0) {
  throw new Error("Could not list repository files for the identity check");
}

const violations: string[] = [];
for (const path of new TextDecoder()
  .decode(tracked.stdout)
  .split("\n")
  .filter(Boolean)) {
  if (
    allowlist.ignoredPaths.includes(path) ||
    allowlist.allowedPaths.includes(path) ||
    ignoredDirectories.some((directory) => path.startsWith(directory))
  ) {
    continue;
  }
  let source: string;
  try {
    source = readFileSync(resolve(root, path), "utf8");
  } catch {
    continue;
  }
  if (source.includes("\0")) {
    continue;
  }
  for (const [index, line] of source.split("\n").entries()) {
    if (!/\bhandy\b/i.test(line)) continue;
    if (allowedPatterns.some((pattern) => pattern.test(line))) continue;
    violations.push(`${path}:${index + 1}: ${line.trim()}`);
  }
}

if (violations.length > 0) {
  console.error("Sona identity check found unclassified Handy references:");
  console.error(violations.join("\n"));
  process.exitCode = 1;
} else {
  console.log("Sona identity check passed");
}
