import { mkdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import { base64UrlDecode, base64UrlEncode, randomId } from "../../cloudflare/sona-companion/src/encoding";
import type { DeviceIdentity } from "./client";
import { KEY_BYTES } from "./keys";

const key32 = z.string().refine((value) => base64UrlDecode(value)?.length === KEY_BYTES, {
  message: "expected 32 base64url bytes",
});

const Identity = z.object({
  device_id: z.string(),
  pairing_secret: key32,
  signing_seed: key32,
});
type Identity = z.infer<typeof Identity>;

export const PairingOffer = z.object({
  candidate_proof: z.string(),
  device_id: z.string(),
  expires_at_utc_ms: z.number().int(),
  fingerprint: z.string(),
  pairing_nonce: z.string(),
  pairing_public_key: z.string(),
  protocol_version: z.literal(1),
  signing_public_key: z.string(),
  vault_id: z.string(),
});
export type PairingOffer = z.infer<typeof PairingOffer>;

const Pending = z.object({
  endpoint: z.string(),
  offer: PairingOffer,
  vault_id: z.string(),
});
export type Pending = z.infer<typeof Pending>;

const Credentials = z.object({
  endpoint: z.string(),
  vault_id: z.string(),
  vault_root: key32,
});
export type Credentials = z.infer<typeof Credentials>;

const FeedObject = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("thought"), revision_id: z.string() }),
  z.object({
    cluster: z.object({ key: z.string(), name: z.string() }),
    kind: z.literal("card"),
    revision_id: z.string(),
    thought_id: z.string(),
  }),
  z.object({ kind: z.literal("other"), revision_id: z.string() }),
]);
export type FeedObject = z.infer<typeof FeedObject>;

/** A derived cache of the vault's heads; delete the file and the next pass rebuilds it. */
const Feed = z.object({
  attempts: z.record(z.object({ count: z.number().int(), next_at_utc_ms: z.number().int() })),
  cursor: z.string().nullable(),
  objects: z.record(FeedObject),
});
export type Feed = z.infer<typeof Feed>;

export const emptyFeed: Feed = { attempts: {}, cursor: null, objects: {} };

export interface FullIdentity extends DeviceIdentity {
  pairingSecret: Uint8Array;
}

/** What `readFile` throws for a missing file; anything else propagates. */
const MissingFile = z.object({ code: z.literal("ENOENT") });

async function readJson<Value>(path: string, schema: z.ZodType<Value>): Promise<Value | null> {
  let text: string;
  try {
    text = await readFile(path, "utf8");
  } catch (error) {
    if (MissingFile.safeParse(error).success) return null;
    throw error;
  }
  return schema.parse(JSON.parse(text));
}

async function writeJson(
  path: string,
  value: Identity | Pending | Credentials | Feed,
  mode: number,
): Promise<void> {
  const scratch = `${path}.tmp`;
  await writeFile(scratch, JSON.stringify(value, null, 2), { mode });
  await rename(scratch, path);
}

function decodeKey(text: string): Uint8Array {
  const bytes = base64UrlDecode(text);
  if (bytes === null || bytes.length !== KEY_BYTES) throw new Error("invalid key material");
  return bytes;
}

/** The sorter's files under one directory: identity, pairing, credentials, feed cache. */
export class StateDir {
  constructor(readonly root: string) {}

  private path(name: string): string {
    return join(this.root, name);
  }

  async init(): Promise<void> {
    await mkdir(this.root, { recursive: true, mode: 0o700 });
  }

  /** The device identity, minted on first use and kept for the life of the pairing. */
  async identity(): Promise<FullIdentity> {
    const stored = await readJson(this.path("identity.json"), Identity);
    if (stored !== null) {
      return {
        deviceId: stored.device_id,
        pairingSecret: decodeKey(stored.pairing_secret),
        signingSeed: decodeKey(stored.signing_seed),
      };
    }
    const fresh = {
      device_id: randomId(),
      pairing_secret: base64UrlEncode(crypto.getRandomValues(new Uint8Array(KEY_BYTES))),
      signing_seed: base64UrlEncode(crypto.getRandomValues(new Uint8Array(KEY_BYTES))),
    };
    await writeJson(this.path("identity.json"), fresh, 0o600);
    return {
      deviceId: fresh.device_id,
      pairingSecret: decodeKey(fresh.pairing_secret),
      signingSeed: decodeKey(fresh.signing_seed),
    };
  }

  pending(): Promise<Pending | null> {
    return readJson(this.path("pending.json"), Pending);
  }

  savePending(pending: Pending): Promise<void> {
    return writeJson(this.path("pending.json"), pending, 0o600);
  }

  clearPending(): Promise<void> {
    return rm(this.path("pending.json"), { force: true });
  }

  credentials(): Promise<Credentials | null> {
    return readJson(this.path("credentials.json"), Credentials);
  }

  saveCredentials(credentials: Credentials): Promise<void> {
    return writeJson(this.path("credentials.json"), credentials, 0o600);
  }

  async feed(): Promise<Feed> {
    return (await readJson(this.path("feed.json"), Feed)) ?? emptyFeed;
  }

  saveFeed(feed: Feed): Promise<void> {
    return writeJson(this.path("feed.json"), feed, 0o600);
  }
}
