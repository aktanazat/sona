import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { isJsonArray, isJsonRecord, isJsonString } from "../src/json";

interface MigrationEntry {
  checksum: string;
  file: string;
  version: string;
}

interface MigrationManifest {
  migrations: MigrationEntry[];
  version: number;
}

const migrationsDirectory = join(
  fileURLToPath(new URL("../migrations/", import.meta.url)),
);
const manifestPath = join(migrationsDirectory, "manifest.json");
const checksumPattern = /sha256:([a-f0-9]{64})/g;
const declaredPattern = /^-- sona-migration-checksum: sha256:([a-f0-9]{64})$/m;
const versionPattern = /^-- sona-migration-version: ([a-z0-9_]+)$/m;
const filePattern = /^(\d{4})_([a-z0-9_]+)\.sql$/;
const auditPattern =
  /INSERT INTO schema_migrations\s*\(version,\s*checksum,\s*applied_at\)\s*VALUES\s*\('([a-z0-9_]+)',\s*'sha256:([a-f0-9]{64})',\s*unixepoch\(\)\s*\*\s*1000\);/m;

function expectedChecksum(source: string): string {
  const canonical = source.replaceAll(checksumPattern, "sha256:__CHECKSUM__");
  return createHash("sha256").update(canonical).digest("hex");
}

function migrationManifest(): MigrationManifest {
  let value: unknown;
  try {
    value = JSON.parse(readFileSync(manifestPath, "utf8"));
  } catch {
    throw new Error("migration manifest is unreadable");
  }
  if (!isJsonRecord(value)) {
    throw new Error("migration manifest must be an object");
  }
  if (value.version !== 1 || !isJsonArray(value.migrations)) {
    throw new Error("migration manifest has an unsupported shape");
  }
  const migrations: MigrationEntry[] = [];
  for (const candidate of value.migrations) {
    if (!isJsonRecord(candidate)) {
      throw new Error("migration manifest has an invalid entry");
    }
    const file = candidate.file;
    const version = candidate.version;
    const checksum = candidate.checksum;
    if (
      !isJsonString(file) ||
      !isJsonString(version) ||
      !isJsonString(checksum)
    ) {
      throw new Error("migration manifest has an invalid entry");
    }
    migrations.push({ file, version, checksum });
  }
  return { version: 1, migrations };
}

function checkMigration(
  name: string,
  entry: MigrationEntry,
  expectedSequence: number,
): void {
  const fileMatch = filePattern.exec(name);
  if (fileMatch === null || Number(fileMatch[1]) !== expectedSequence) {
    throw new Error(`${name} breaks forward-only numeric ordering`);
  }
  const fileVersion = `${fileMatch[1]}_${fileMatch[2]}`;
  if (entry.file !== name || entry.version !== fileVersion) {
    throw new Error(`${name} does not match its immutable manifest entry`);
  }

  const source = readFileSync(join(migrationsDirectory, name), "utf8");
  const declared = declaredPattern.exec(source)?.[1];
  const declaredVersion = versionPattern.exec(source)?.[1];
  const audit = auditPattern.exec(source);
  if (
    declared === undefined ||
    declaredVersion === undefined ||
    audit === null
  ) {
    throw new Error(`${name} is missing a required migration declaration`);
  }
  if (
    declaredVersion !== fileVersion ||
    audit[1] !== fileVersion ||
    audit[2] !== declared
  ) {
    throw new Error(`${name} disagrees across filename, header, or audit row`);
  }

  const allChecksums = [...source.matchAll(checksumPattern)].map(
    (match) => match[1],
  );
  if (
    allChecksums.length !== 2 ||
    allChecksums.some((value) => value !== declared)
  ) {
    throw new Error(
      `${name} must repeat one checksum in its header and audit row`,
    );
  }
  const actual = expectedChecksum(source);
  if (actual !== declared || entry.checksum !== `sha256:${declared}`) {
    throw new Error(
      `${name} checksum does not match immutable migration history`,
    );
  }
}

const manifest = migrationManifest();
const migrations = readdirSync(migrationsDirectory)
  .filter((name) => filePattern.test(name))
  .sort();

if (
  migrations.length === 0 ||
  migrations.length !== manifest.migrations.length
) {
  throw new Error("migration files and immutable manifest disagree");
}

for (const [index, migration] of migrations.entries()) {
  const entry = manifest.migrations[index];
  if (entry === undefined) throw new Error("migration manifest is incomplete");
  checkMigration(migration, entry, index + 1);
}

console.log(
  `verified ${migrations.length} immutable forward-only migration checksum${migrations.length === 1 ? "" : "s"}`,
);
