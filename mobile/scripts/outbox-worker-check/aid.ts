/**
 * Helpers for the outbox check against a local copy of the real companion Worker.
 *
 *   bun aid.ts bootstrap <identity.json> <vault-id> <worker-url> <secret>
 *     registers the phone identity the Swift check minted as the vault's first device.
 *   bun aid.ts proxy <port> <worker-url>
 *     forwards to the Worker with a little latency, as a phone network has, so two
 *     outbox passes would overlap; POST /__check/drop-next-chunk makes the next chunk
 *     upload fail at the transport, and GET /__check/last-upload names the last upload
 *     created.
 */
import { createPrivateKey, createPublicKey, sign } from "node:crypto";
import { z } from "zod";

import { canonicalBootstrapBytes } from "../../../cloudflare/sona-companion/src/crypto";

const [mode, ...args] = process.argv.slice(2);

function base64url(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("base64url");
}

function keyFromSeed(seed: Buffer, curve: "ed25519" | "x25519") {
  const prefix = curve === "ed25519" ? "302e020100300506032b657004220420" : "302e020100300506032b656e04220420";
  const privateKey = createPrivateKey({
    key: Buffer.concat([Buffer.from(prefix, "hex"), seed]),
    format: "der",
    type: "pkcs8",
  });
  const publicKey = new Uint8Array(createPublicKey(privateKey).export({ format: "der", type: "spki" }).subarray(-32));
  return { privateKey, publicKey };
}

if (mode === "bootstrap") {
  const [identityPath, vaultId, worker, secret] = args;
  if (identityPath === undefined || vaultId === undefined || worker === undefined || secret === undefined)
    throw new Error("usage: aid.ts bootstrap <identity.json> <vault-id> <worker-url> <secret>");
  const identity = z
    .object({ deviceId: z.string(), signingSeedBase64URL: z.string(), pairingSecretBase64URL: z.string() })
    .parse(JSON.parse(await Bun.file(identityPath).text()));
  const signing = keyFromSeed(Buffer.from(identity.signingSeedBase64URL, "base64url"), "ed25519");
  const pairing = keyFromSeed(Buffer.from(identity.pairingSecretBase64URL, "base64url"), "x25519");
  const signature = sign(
    null,
    canonicalBootstrapBytes({
      audience: "sona-companion",
      vaultId,
      deviceId: identity.deviceId,
      signingPublicKey: signing.publicKey,
      pairingPublicKey: pairing.publicKey,
    }),
    signing.privateKey,
  );
  const body = JSON.stringify({
    version: 1,
    vaultId,
    deviceId: identity.deviceId,
    signingPublicKey: base64url(signing.publicKey),
    pairingPublicKey: base64url(pairing.publicKey),
    selfSignature: base64url(signature),
  });
  const response = await fetch(new URL("/v1/bootstrap/device", worker), {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-sona-bootstrap-secret": secret,
      "x-sona-idempotency-key": `bootstrap_${identity.deviceId}`,
    },
    body,
  });
  console.log("bootstrap", response.status, await response.text());
  if (!response.ok) process.exit(1);
} else if (mode === "proxy") {
  const [port, worker] = args;
  if (port === undefined || worker === undefined) throw new Error("usage: aid.ts proxy <port> <worker-url>");
  let dropNextChunk = false;
  let lastUpload = "";
  /* One request at a time, in arrival order, as over one slow link: two identical
   * requests then meet the Worker one after the other (a replay) instead of together
   * (a 503 that would end the second pass before the race). */
  let link: Promise<unknown> = Promise.resolve();
  Bun.serve({
    hostname: "127.0.0.1",
    port: Number(port),
    async fetch(request) {
      const url = new URL(request.url);
      if (url.pathname === "/__check/drop-next-chunk") {
        dropNextChunk = true;
        return new Response("ok");
      }
      if (url.pathname === "/__check/last-upload") return new Response(lastUpload);
      const isChunk = request.method === "PUT" && url.pathname.includes("/chunks/");
      if (isChunk && dropNextChunk) {
        dropNextChunk = false;
        return Response.error();
      }
      const body = request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer();
      const turn = link.then(async () => {
        await Bun.sleep(30);
        const forwarded = await fetch(new URL(url.pathname + url.search, worker), {
          method: request.method,
          headers: request.headers,
          body,
        });
        return { forwarded, bytes: await forwarded.arrayBuffer() };
      });
      link = turn.catch(() => undefined);
      const { forwarded, bytes } = await turn;
      if (request.method === "POST" && url.pathname === "/v1/uploads" && forwarded.ok) {
        lastUpload = z.object({ upload_id: z.string() }).parse(JSON.parse(new TextDecoder().decode(bytes))).upload_id;
      }
      /* fetch already decoded the body, so its encoding and length no longer apply. */
      const headers = new Headers(forwarded.headers);
      headers.delete("content-encoding");
      headers.delete("content-length");
      return new Response(bytes, { status: forwarded.status, headers });
    },
  });
  console.log(`proxy on ${port}`);
} else {
  throw new Error("usage: aid.ts bootstrap|proxy ...");
}
