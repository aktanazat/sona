import { afterEach, describe, expect, it, vi } from "vitest";

import fixture from "../fixtures/crypto-v1.json";

function decodeBase64Url(value: string): Uint8Array {
  const padded =
    value.replaceAll("-", "+").replaceAll("_", "/") +
    "=".repeat((4 - (value.length % 4)) % 4);
  const binary = atob(padded);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1)
    bytes[index] = binary.charCodeAt(index);
  return bytes;
}

function encodeBase64Url(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary)
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

async function sha256(value: Uint8Array): Promise<string> {
  return encodeBase64Url(
    new Uint8Array(await crypto.subtle.digest("SHA-256", value)),
  );
}

function requestPath(input: RequestInfo | URL): string {
  if (input instanceof URL) return input.pathname;
  if (input instanceof Request) return new URL(input.url).pathname;
  return new URL(input, location.origin).pathname;
}

afterEach(() => {
  vi.unstubAllGlobals();
  document.body.replaceChildren();
  history.replaceState(null, "", "/");
});

describe("encrypted share viewer", () => {
  it("removes the fragment, decrypts only the share bundle, and renders hostile Markdown as text", async () => {
    const shareId = fixture.share_aes_gcm_hkdf.share_id;
    const root = fixture.share_aes_gcm_hkdf.root;
    const manifest = decodeBase64Url(
      fixture.share_aes_gcm_hkdf.manifest.ciphertext,
    );
    const chunk = decodeBase64Url(fixture.share_aes_gcm_hkdf.chunk.ciphertext);
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL): Promise<Response> => {
        const path = requestPath(input);
        if (path === `/v1/shares/${shareId}/manifest`) {
          return new Response(
            JSON.stringify({
              version: 1,
              share: {
                share_id: shareId,
                crypto_version: 1,
                manifest_sha256: await sha256(manifest),
                chunk_count: 1,
                total_bytes: chunk.length,
                writer_signature: fixture.canonical_request.signature,
              },
              manifest: fixture.share_aes_gcm_hkdf.manifest.ciphertext,
              chunks: [
                { index: 0, size: chunk.length, sha256: await sha256(chunk) },
              ],
            }),
            { headers: { "content-type": "application/json" } },
          );
        }
        if (path === `/v1/shares/${shareId}/chunks/0`)
          return new Response(chunk);
        return new Response(null, { status: 404 });
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    document.body.innerHTML = `
      <h1 id="share-title"></h1>
      <p id="status"></p>
      <a id="download-file"></a>
      <article id="markdown-content" hidden></article>
    `;
    history.replaceState(null, "", `/s/${shareId}#v=1&k=${root}`);

    // The module auto-boots, so it must load only after this test installs its DOM and same-origin fetch boundary.
    const viewer = await import("../public/viewer.js?viewer-behavior-test");
    document.dispatchEvent(new Event("DOMContentLoaded"));

    await vi.waitFor(() =>
      expect(document.getElementById("status")?.textContent).toBe(
        "Decrypted in this browser.",
      ),
    );

    expect(location.hash).toBe("");
    expect(document.getElementById("share-title")?.textContent).toBe("Fixture");
    expect(document.querySelector("#markdown-content h2")?.textContent).toBe(
      "Fixture",
    );
    expect(fetchMock).toHaveBeenCalledTimes(2);

    const content = document.getElementById("markdown-content");
    if (content === null) throw new Error("viewer content element is missing");
    viewer.renderMarkdown(
      content,
      "<img src=x onerror=alert(1)>\n[script](javascript:alert(1))\n[safe](https://example.test/path)",
    );
    expect(content.querySelector("img")).toBeNull();
    expect(content.querySelector("script")).toBeNull();
    expect(content.querySelectorAll("a")).toHaveLength(1);
    expect(content.querySelector("a")?.href).toBe("https://example.test/path");
    expect(content.textContent).toContain("<img src=x onerror=alert(1)>");
    expect(content.textContent).toContain("[script](javascript:alert(1))");
    content.replaceChildren();
    expect(() =>
      viewer.renderMarkdown(content, "x\n".repeat(10_001)),
    ).toThrow();
    expect(content.childElementCount).toBe(0);
    expect(() => viewer.parseFragment(`#v=1&k=${root}&k=${root}`)).toThrow();
  });

  it("fails closed on an unexpected bundle field before requesting ciphertext chunks", async () => {
    const shareId = fixture.share_aes_gcm_hkdf.share_id;
    const root = fixture.share_aes_gcm_hkdf.root;
    const manifest = decodeBase64Url(
      fixture.share_aes_gcm_hkdf.manifest.ciphertext,
    );
    const chunk = decodeBase64Url(fixture.share_aes_gcm_hkdf.chunk.ciphertext);
    const fetchMock = vi.fn(
      async (): Promise<Response> =>
        new Response(
          JSON.stringify({
            version: 1,
            unexpected: true,
            share: {
              share_id: shareId,
              crypto_version: 1,
              manifest_sha256: await sha256(manifest),
              chunk_count: 1,
              total_bytes: chunk.length,
              writer_signature: fixture.canonical_request.signature,
            },
            manifest: fixture.share_aes_gcm_hkdf.manifest.ciphertext,
            chunks: [
              { index: 0, size: chunk.length, sha256: await sha256(chunk) },
            ],
          }),
          { headers: { "content-type": "application/json" } },
        ),
    );
    vi.stubGlobal("fetch", fetchMock);
    document.body.innerHTML = `
      <h1 id="share-title"></h1>
      <p id="status"></p>
      <a id="download-file"></a>
      <article id="markdown-content" hidden></article>
    `;
    history.replaceState(null, "", `/s/${shareId}#v=1&k=${root}`);

    // This distinct module instance is necessary because the viewer boot runs when its module loads.
    await import("../public/viewer.js?viewer-schema-test");
    document.dispatchEvent(new Event("DOMContentLoaded"));

    await vi.waitFor(() =>
      expect(document.getElementById("status")?.textContent).toBe(
        "This share cannot be opened.",
      ),
    );
    expect(
      document.getElementById("markdown-content")?.hasAttribute("hidden"),
    ).toBe(true);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});

// The documents `share_document.rs` writes for its golden meeting, verbatim.
const NOTES = [
  '{"heading":"Summary","blocks":[{"type":"paragraph","text":"We agreed on the May launch."}]}',
  '{"heading":"What was covered","blocks":[{"type":"item","text":"Launch date","meta":"Moved to May","time":null}]}',
  '{"heading":"Decisions","blocks":[{"type":"item","text":"Ship the redesign in May.","meta":null,"time":null}]}',
  '{"heading":"Action items","blocks":[{"type":"item","text":"Draft the launch note","meta":"Priya · Friday","time":null}]}',
  '{"heading":"Risks","blocks":[{"type":"item","text":"The venue may fall through.","meta":null,"time":null}]}',
  '{"heading":"Your own notes","blocks":[{"type":"paragraph","text":"Ask Priya about the venue."},{"type":"paragraph","text":"Bring the deck."}]}',
].join(",");
const MOMENTS =
  '{"heading":"Notes in the moment","blocks":[{"type":"item","text":"Ask about budget","meta":null,"time":"0:30"}]}';
const TRANSCRIPT =
  '{"heading":"Transcript","blocks":[{"type":"item","text":"We ship in May. The venue is booked.","meta":"Priya","time":"0:00"},{"type":"item","text":"Who writes the note?","meta":"Aktan","time":"1:05"}]}';
const NOTE_HEADINGS = [
  "Summary",
  "What was covered",
  "Decisions",
  "Action items",
  "Risks",
  "Your own notes",
];
const NOTE_METAS = ["Moved to May", "Priya · Friday"];

function documentJson(include: string, sections: string[]): string {
  return `{"version":1,"title":"Design sync","include":"${include}","notes_out_of_date":false,"sections":[${sections.join(",")}]}`;
}

const textEncoder = new TextEncoder();

function record(...values: (string | number)[]): Uint8Array {
  const parts: Uint8Array[] = [];
  for (const value of values) {
    const bytes = textEncoder.encode(String(value));
    const length = new Uint8Array(4);
    new DataView(length.buffer).setUint32(0, bytes.length, false);
    parts.push(length, bytes);
  }
  const output = new Uint8Array(
    parts.reduce((total, part) => total + part.length, 0),
  );
  let offset = 0;
  for (const part of parts) {
    output.set(part, offset);
    offset += part.length;
  }
  return output;
}

// Seals one payload the way the Mac core's `seal_share_payload` does.
async function seal(
  root: Uint8Array,
  shareId: string,
  domain: "manifest" | "chunk",
  plaintext: Uint8Array,
): Promise<Uint8Array> {
  const input = await crypto.subtle.importKey("raw", root, "HKDF", false, [
    "deriveKey",
  ]);
  const key = await crypto.subtle.deriveKey(
    {
      name: "HKDF",
      hash: "SHA-256",
      salt: textEncoder.encode("sona-share-v1"),
      info: record("sona-share-key-v1", shareId, 0, 1, domain),
    },
    input,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt"],
  );
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const sealed = new Uint8Array(
    await crypto.subtle.encrypt(
      {
        name: "AES-GCM",
        iv: nonce,
        additionalData: record("sona-share-aad-v1", shareId, 0, 1, domain),
        tagLength: 128,
      },
      key,
      plaintext,
    ),
  );
  const payload = new Uint8Array(12 + sealed.length);
  payload.set(nonce);
  payload.set(sealed, 12);
  return payload;
}

function texts(container: Element, selector: string): (string | null)[] {
  return [...container.querySelectorAll(selector)].map(
    (element) => element.textContent,
  );
}

describe("shared notes document", () => {
  const cases: [string, string[], string[], string[]][] = [
    ["notes", [NOTES], NOTE_HEADINGS, NOTE_METAS],
    [
      "notes_and_transcript",
      [NOTES, TRANSCRIPT],
      [...NOTE_HEADINGS, "Transcript"],
      [...NOTE_METAS, "0:00 · Priya", "1:05 · Aktan"],
    ],
    [
      "everything",
      [NOTES, MOMENTS, TRANSCRIPT],
      [...NOTE_HEADINGS, "Notes in the moment", "Transcript"],
      [...NOTE_METAS, "0:30", "0:00 · Priya", "1:05 · Aktan"],
    ],
  ];
  for (const [include, sections, headings, metas] of cases) {
    it(`renders the ${include} document section by section`, async () => {
      // The module boots on load; loaded here, it finds no page and stays idle.
      const viewer = await import("../public/viewer.js?viewer-document-test");
      const container = document.createElement("article");
      viewer.renderShareDocument(
        container,
        viewer.parseShareDocument(documentJson(include, sections)),
      );
      expect(texts(container, "h2")).toEqual(headings);
      expect(texts(container, ".share-meta")).toEqual(metas);
      expect(texts(container, ".share-paragraph")).toEqual([
        "We agreed on the May launch.",
        "Ask Priya about the venue.",
        "Bring the deck.",
      ]);
    });
  }

  it("fails closed outside the document shape, and says when a share is empty or stale", async () => {
    // The module boots on load; loaded here, it finds no page and stays idle.
    const viewer = await import("../public/viewer.js?viewer-document-test");
    const valid = documentJson("notes", [NOTES]);
    expect(() =>
      viewer.parseShareDocument(
        valid.replace('"version":1,', '"version":1,"extra":true,'),
      ),
    ).toThrow();
    expect(() =>
      viewer.parseShareDocument(
        valid.replace('"type":"paragraph"', '"type":"html"'),
      ),
    ).toThrow();
    expect(() =>
      viewer.parseShareDocument(valid.replace('"meta":null', '"meta":1')),
    ).toThrow();
    expect(() =>
      viewer.parseShareDocument(valid.replace('"time":null', '"time":"soon"')),
    ).toThrow();
    const container = document.createElement("article");
    viewer.renderShareDocument(
      container,
      viewer.parseShareDocument(documentJson("everything", [])),
    );
    expect(container.textContent).toBe("Nothing was shared in this link.");
    viewer.renderShareDocument(
      container,
      viewer.parseShareDocument(
        valid.replace('"notes_out_of_date":false', '"notes_out_of_date":true'),
      ),
    );
    expect(container.firstElementChild?.textContent).toBe(
      "These notes were written before the transcript last changed.",
    );
  });

  it("decrypts a notes document in the browser and keeps hostile text as text", async () => {
    const shareId = fixture.share_aes_gcm_hkdf.share_id;
    const rootText = fixture.share_aes_gcm_hkdf.root;
    const root = decodeBase64Url(rootText);
    const body = textEncoder.encode(
      documentJson("notes", [
        '{"heading":"Summary","blocks":[{"type":"paragraph","text":"<img src=x onerror=alert(1)>"}]}',
      ]),
    );
    const manifest = await seal(
      root,
      shareId,
      "manifest",
      textEncoder.encode(
        JSON.stringify({
          version: 1,
          kind: "notes_document",
          source_format: "sona-share-document-v1",
          title: "Design sync",
          chunk_count: 1,
          plaintext_bytes: body.length,
        }),
      ),
    );
    const chunk = await seal(root, shareId, "chunk", body);
    const manifestSha256 = await sha256(manifest);
    const chunkSha256 = await sha256(chunk);
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
        const path = requestPath(input);
        if (path === `/v1/shares/${shareId}/manifest`)
          return new Response(
            JSON.stringify({
              version: 1,
              share: {
                share_id: shareId,
                crypto_version: 1,
                manifest_sha256: manifestSha256,
                chunk_count: 1,
                total_bytes: chunk.length,
                writer_signature: fixture.canonical_request.signature,
              },
              manifest: encodeBase64Url(manifest),
              chunks: [{ index: 0, size: chunk.length, sha256: chunkSha256 }],
            }),
            { headers: { "content-type": "application/json" } },
          );
        if (path === `/v1/shares/${shareId}/chunks/0`)
          return new Response(chunk);
        return new Response(null, { status: 404 });
      }),
    );
    document.body.innerHTML = `
      <h1 id="share-title"></h1>
      <p id="status"></p>
      <a id="download-file"></a>
      <article id="markdown-content" hidden></article>
    `;
    history.replaceState(null, "", `/s/${shareId}#v=1&k=${rootText}`);

    // A distinct module instance, loaded only after this test installs its DOM and fetch boundary.
    await import("../public/viewer.js?viewer-document-boot-test");
    document.dispatchEvent(new Event("DOMContentLoaded"));

    await vi.waitFor(() =>
      expect(document.getElementById("status")?.textContent).toBe(
        "Decrypted in this browser.",
      ),
    );
    expect(document.getElementById("share-title")?.textContent).toBe(
      "Design sync",
    );
    expect(document.querySelector("#markdown-content img")).toBeNull();
    expect(
      document.querySelector("#markdown-content .share-paragraph")?.textContent,
    ).toBe("<img src=x onerror=alert(1)>");
  });
});
