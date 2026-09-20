import { deriveAesGcmKey, record } from "../../cloudflare/sona-companion/src/crypto";
import {
  base64UrlDecode,
  concatBytes,
  equalBytes,
  utf8,
} from "../../cloudflare/sona-companion/src/encoding";

/* RFC 8410 PKCS#8 headers for a raw 32-byte private key; WebCrypto imports no
 * bare seed, so the seed the identity file holds is wrapped on every use. */
const ED25519_PKCS8_PREFIX = hexBytes("302e020100300506032b657004220420");
const X25519_PKCS8_PREFIX = hexBytes("302e020100300506032b656e04220420");

export const KEY_BYTES = 32;
const AES_GCM_NONCE_BYTES = 12;
const AES_GCM_TAG_BYTES = 16;
const PAIRING_ENVELOPE_VERSION = "1";

function hexBytes(hex: string): Uint8Array {
  const bytes = new Uint8Array(hex.length / 2);
  for (let index = 0; index < bytes.length; index += 1) {
    bytes[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16);
  }
  return bytes;
}

async function importPrivateKey(
  prefix: Uint8Array,
  raw: Uint8Array,
  algorithm: "Ed25519" | "X25519",
  usages: KeyUsage[],
): Promise<CryptoKey> {
  if (raw.length !== KEY_BYTES) throw new Error(`invalid ${algorithm} private key`);
  return crypto.subtle.importKey(
    "pkcs8",
    concatBytes([prefix, raw]),
    { name: algorithm },
    true,
    usages,
  );
}

/* A private OKP key exported as JWK carries its public half in `x`, which is the
 * one way WebCrypto derives a public key from a seed. */
async function publicKeyOf(privateKey: CryptoKey): Promise<Uint8Array> {
  const jwk = await crypto.subtle.exportKey("jwk", privateKey);
  const raw = jwk.x === undefined ? null : base64UrlDecode(jwk.x);
  if (raw === null || raw.length !== KEY_BYTES) throw new Error("invalid public key");
  return raw;
}

export async function ed25519PublicKey(seed: Uint8Array): Promise<Uint8Array> {
  return publicKeyOf(await importPrivateKey(ED25519_PKCS8_PREFIX, seed, "Ed25519", ["sign"]));
}

export async function signEd25519(
  seed: Uint8Array,
  message: Uint8Array,
): Promise<Uint8Array> {
  const key = await importPrivateKey(ED25519_PKCS8_PREFIX, seed, "Ed25519", ["sign"]);
  return new Uint8Array(await crypto.subtle.sign("Ed25519", key, message));
}

export async function x25519PublicKey(secret: Uint8Array): Promise<Uint8Array> {
  return publicKeyOf(
    await importPrivateKey(X25519_PKCS8_PREFIX, secret, "X25519", ["deriveBits"]),
  );
}

/* WebCrypto rejects an all-zero shared secret itself, which is the contributory
 * check `crypto.rs` makes by hand. */
export async function x25519SharedSecret(
  secret: Uint8Array,
  peerPublicKey: Uint8Array,
): Promise<Uint8Array> {
  if (peerPublicKey.length !== KEY_BYTES) throw new Error("invalid X25519 public key");
  const privateKey = await importPrivateKey(X25519_PKCS8_PREFIX, secret, "X25519", [
    "deriveBits",
  ]);
  const publicKey = await crypto.subtle.importKey("raw", peerPublicKey, { name: "X25519" }, false, []);
  const bits = await crypto.subtle.deriveBits(
    { name: "X25519", public: publicKey },
    privateKey,
    KEY_BYTES * 8,
  );
  return new Uint8Array(bits);
}

/** Split a u32be length-prefixed record into its fields, or null when malformed. */
export function decodeRecord(bytes: Uint8Array): Uint8Array[] | null {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const fields: Uint8Array[] = [];
  let offset = 0;
  while (offset < bytes.length) {
    if (offset + 4 > bytes.length) return null;
    const length = view.getUint32(offset);
    offset += 4;
    if (offset + length > bytes.length) return null;
    fields.push(bytes.subarray(offset, offset + length));
    offset += length;
  }
  return fields;
}

/**
 * Open the desktop's pairing envelope and return the authenticated 32-byte vault root.
 *
 * Mirrors `SonaCrypto.swift::openPairingEnvelope` field for field: the sorter is a
 * candidate like the phone, so it only ever opens one of these.
 */
export async function openPairingEnvelope(
  recipientSecret: Uint8Array,
  envelope: Uint8Array,
): Promise<Uint8Array> {
  const fields = decodeRecord(envelope);
  if (
    fields === null ||
    fields.length !== 5 ||
    !equalBytes(fields[0] ?? new Uint8Array(), utf8("sona-pairing-envelope-v1"))
  ) {
    throw new Error("invalid pairing envelope");
  }
  if (!equalBytes(fields[1] ?? new Uint8Array(), utf8(PAIRING_ENVELOPE_VERSION))) {
    throw new Error("unsupported pairing envelope version");
  }
  const ephemeralPublicKey = fields[2] ?? new Uint8Array();
  const nonce = fields[3] ?? new Uint8Array();
  const ciphertext = fields[4] ?? new Uint8Array();
  if (
    ephemeralPublicKey.length !== KEY_BYTES ||
    nonce.length !== AES_GCM_NONCE_BYTES ||
    ciphertext.length !== KEY_BYTES + AES_GCM_TAG_BYTES
  ) {
    throw new Error("invalid pairing envelope");
  }
  const recipientPublicKey = await x25519PublicKey(recipientSecret);
  const shared = await x25519SharedSecret(recipientSecret, ephemeralPublicKey);
  const key = await deriveAesGcmKey(
    shared,
    utf8("sona-pairing-envelope-v1"),
    record("sona-pairing-envelope-key-v1", recipientPublicKey, ephemeralPublicKey),
  );
  const aad = record(
    "sona-pairing-envelope-aad-v1",
    PAIRING_ENVELOPE_VERSION,
    recipientPublicKey,
    ephemeralPublicKey,
    nonce,
  );
  const vaultRoot = new Uint8Array(
    await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: nonce, additionalData: aad, tagLength: 128 },
      key,
      ciphertext,
    ),
  );
  if (vaultRoot.length !== KEY_BYTES) throw new Error("invalid pairing envelope");
  return vaultRoot;
}
