import { canonicalPairCandidateBytes } from "../../cloudflare/sona-companion/src/crypto";
import {
  base64UrlDecode,
  base64UrlEncode,
  sha256Base64Url,
} from "../../cloudflare/sona-companion/src/encoding";
import { ApiError, CompanionClient, type SelfDevice } from "./client";
import { ed25519PublicKey, openPairingEnvelope, signEd25519, x25519PublicKey } from "./keys";
import type { SorterModel } from "./sort";
import {
  type Credentials,
  type FullIdentity,
  type PairingOffer,
  type Pending,
  StateDir,
} from "./state";
import { Sorter } from "./vault";

const AUDIENCE = "sona-companion";
const PROTOCOL_VERSION = 1;
const OFFER_LIFETIME_MS = 15 * 60 * 1000;
const APPROVAL_POLL_MS = 5 * 1000;
const DEFAULT_INTERVAL_MS = 30 * 1000;
const DEFAULT_LLM_BASE_URL = "https://api.orcarouter.ai/v1";
const DEFAULT_LLM_MODEL = "google/gemma-4-26b-a4b-it";

const USAGE = `usage:
  bun src/main.ts pair <endpoint> <vault_id>   mint an offer, wait for the desktop to approve it
  bun src/main.ts once                          sync the vault and file unsorted thoughts once
  bun src/main.ts run                           do that every SONA_SORTER_INTERVAL_MS (default 30000)

environment:
  SONA_SORTER_STATE          state directory (default ./state)
  SONA_SORTER_LLM_API_KEY    OpenAI-compatible API key (once/run)
  SONA_SORTER_LLM_BASE_URL   default ${DEFAULT_LLM_BASE_URL}
  SONA_SORTER_LLM_MODEL      default ${DEFAULT_LLM_MODEL}`;

function log(line: string): void {
  console.error(`${new Date().toISOString()} ${line}`);
}

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (value === undefined || value === "") throw new Error(`${name} is not set`);
  return value;
}

function modelFromEnv(): SorterModel {
  return {
    apiKey: requiredEnv("SONA_SORTER_LLM_API_KEY"),
    baseUrl: process.env.SONA_SORTER_LLM_BASE_URL || DEFAULT_LLM_BASE_URL,
    model: process.env.SONA_SORTER_LLM_MODEL || DEFAULT_LLM_MODEL,
  };
}

/** The candidate record `runtime.rs::pairing_offer` mints, signed by this device. */
async function mintOffer(
  client: CompanionClient,
  identity: FullIdentity,
  vaultId: string,
): Promise<PairingOffer> {
  const signingPublicKey = await ed25519PublicKey(identity.signingSeed);
  const pairingPublicKey = await x25519PublicKey(identity.pairingSecret);
  const pairingNonce = crypto.getRandomValues(new Uint8Array(16));
  const expiresAt = client.nowUtcMs() + OFFER_LIFETIME_MS;
  const record = canonicalPairCandidateBytes({
    audience: AUDIENCE,
    candidateDeviceId: identity.deviceId,
    candidatePairingPublicKey: pairingPublicKey,
    candidateSigningPublicKey: signingPublicKey,
    expiresAt,
    pairingNonce,
    vaultId,
  });
  return {
    candidate_proof: base64UrlEncode(await signEd25519(identity.signingSeed, record)),
    device_id: identity.deviceId,
    expires_at_utc_ms: expiresAt,
    fingerprint: (await sha256Base64Url(record)).slice(0, 12),
    pairing_nonce: base64UrlEncode(pairingNonce),
    pairing_public_key: base64UrlEncode(pairingPublicKey),
    protocol_version: PROTOCOL_VERSION,
    signing_public_key: base64UrlEncode(signingPublicKey),
    vault_id: vaultId,
  };
}

/**
 * The Worker's record of this device must match the offer the desktop was shown
 * before its envelope is opened; the same checks `Pairing.acceptApproval` makes.
 */
async function acceptApproval(
  identity: FullIdentity,
  pending: Pending,
  device: SelfDevice,
): Promise<Credentials | null> {
  const { offer } = pending;
  if (
    device.device_id !== identity.deviceId ||
    device.status !== "active" ||
    device.signing_public_key !== offer.signing_public_key ||
    device.pairing_public_key !== offer.pairing_public_key ||
    device.protocol_version !== PROTOCOL_VERSION
  ) {
    throw new Error("the Worker's record of this device does not match the offer");
  }
  if (device.envelope === null) return null;
  const envelope = base64UrlDecode(device.envelope);
  if (envelope === null) throw new Error("the approval envelope is not base64url");
  const vaultRoot = await openPairingEnvelope(identity.pairingSecret, envelope);
  return {
    endpoint: pending.endpoint,
    vault_id: pending.vault_id,
    vault_root: base64UrlEncode(vaultRoot),
  };
}

/* The Worker learns a candidate only when the desktop approves it, so until then
 * every signed request answers 401 unauthorized: that is "not yet", not a fault. */
async function selfDeviceIfKnown(client: CompanionClient): Promise<SelfDevice | null> {
  try {
    return await client.selfDevice();
  } catch (error) {
    if (error instanceof ApiError && error.code === "unauthorized") return null;
    throw error;
  }
}

async function pair(state: StateDir, endpoint: string, vaultId: string): Promise<void> {
  const identity = await state.identity();
  const client = new CompanionClient(endpoint, identity, vaultId);
  await client.syncClock();
  const offer = await mintOffer(client, identity, vaultId);
  const pending: Pending = { endpoint, offer, vault_id: vaultId };
  await state.savePending(pending);
  console.log(JSON.stringify(offer));
  log(`offer fingerprint ${offer.fingerprint}; paste the line above into Sona's Cloud Sync panel`);
  for (;;) {
    if (client.nowUtcMs() >= offer.expires_at_utc_ms) {
      throw new Error("the offer expired before the desktop approved it; run pair again");
    }
    const device = await selfDeviceIfKnown(client);
    const credentials = device === null ? null : await acceptApproval(identity, pending, device);
    if (credentials !== null) {
      await state.saveCredentials(credentials);
      await state.clearPending();
      log(`paired as device ${identity.deviceId} with vault ${vaultId}`);
      return;
    }
    await Bun.sleep(APPROVAL_POLL_MS);
  }
}

async function sorter(state: StateDir): Promise<Sorter> {
  const credentials = await state.credentials();
  if (credentials === null) throw new Error("not paired; run `pair <endpoint> <vault_id>` first");
  const identity = await state.identity();
  const client = new CompanionClient(credentials.endpoint, identity, credentials.vault_id);
  await client.syncClock();
  const vaultRoot = base64UrlDecode(credentials.vault_root);
  if (vaultRoot === null) throw new Error("credentials.json holds no vault root");
  return new Sorter({
    client,
    identity,
    keys: { vaultId: credentials.vault_id, vaultRoot },
    log,
    model: modelFromEnv(),
    state,
  });
}

async function once(state: StateDir): Promise<void> {
  const result = await (await sorter(state)).pass();
  log(`synced ${result.synced} changes, filed ${result.sorted} thoughts`);
}

/* A revoked device cannot recover by retrying; anything else is logged and tried
 * again next interval, so a Worker or model outage never stops the loop. */
async function run(state: StateDir): Promise<void> {
  const intervalMs = Number(process.env.SONA_SORTER_INTERVAL_MS || DEFAULT_INTERVAL_MS);
  if (!Number.isInteger(intervalMs) || intervalMs <= 0) {
    throw new Error("SONA_SORTER_INTERVAL_MS must be a positive integer");
  }
  const worker = await sorter(state);
  for (;;) {
    try {
      const result = await worker.pass();
      if (result.synced > 0 || result.sorted > 0) {
        log(`synced ${result.synced} changes, filed ${result.sorted} thoughts`);
      }
    } catch (error) {
      if (error instanceof ApiError && error.code === "revoked_device") throw error;
      log(`pass failed: ${String(error)}`);
    }
    await Bun.sleep(intervalMs);
  }
}

async function main(argv: readonly string[]): Promise<void> {
  const state = new StateDir(process.env.SONA_SORTER_STATE || "state");
  await state.init();
  const [command, endpoint, vaultId] = argv;
  if (command === "pair" && endpoint !== undefined && vaultId !== undefined) {
    return pair(state, endpoint, vaultId);
  }
  if (command === "once" && argv.length === 1) return once(state);
  if (command === "run" && argv.length === 1) return run(state);
  throw new Error(USAGE);
}

try {
  await main(process.argv.slice(2));
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
}
