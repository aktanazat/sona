// The sorter signs every request with a seed wrapped into PKCS#8 by hand; this
// checks that wrapping against the fixture the Worker and desktop are tested on.
import { describe, expect, test } from "bun:test";
import fixture from "../../cloudflare/sona-companion/fixtures/crypto-v1.json";
import { base64UrlDecode, base64UrlEncode } from "../../cloudflare/sona-companion/src/encoding";
import { ed25519PublicKey, signEd25519 } from "../src/keys";

/* RFC 8032 test vector 1: the seed `scripts/generate-crypto-fixture.ts` signs with. */
const SEED = Uint8Array.from(
  Buffer.from("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "hex"),
);

function decode(text: string): Uint8Array {
  const bytes = base64UrlDecode(text);
  if (bytes === null) throw new Error(`fixture field is not base64url: ${text}`);
  return bytes;
}

describe("ed25519 over the fixture seed", () => {
  test("derives the fixture's public key", async () => {
    expect(base64UrlEncode(await ed25519PublicKey(SEED))).toBe(
      fixture.canonical_request.ed25519_public_key,
    );
  });

  test("signs the canonical request bytes as the fixture does", async () => {
    const signature = await signEd25519(SEED, decode(fixture.canonical_request.bytes));
    expect(base64UrlEncode(signature)).toBe(fixture.canonical_request.signature);
  });
});
