const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
const MAX_VIEWER_PLAINTEXT = 16 * 1024 * 1024;
const MAX_RENDER_BLOCKS = 10_000;
const MAX_RENDER_INLINE_TOKENS = 10_000;
const MAX_RENDER_LINE_CHARS = 256 * 1024;
const inlineToken = /\*\*([^*]+)\*\*|`([^`]+)`|\[([^\]]+)\]\(([^)]+)\)/gu;
const MAX_VIEWER_SHARE_CIPHERTEXT = 256 * 1024 * 1024;
const ENCRYPTED_PAYLOAD_OVERHEAD = 12 + 16;
const MAX_CONCURRENT_CHUNK_REQUESTS = 8;
const MAX_TITLE_CHARS = 240;
const MAX_DOCUMENT_SECTIONS = 32;
// `share_document.rs` in the Mac core writes this document; every bound
// below has its twin there.
const DOCUMENT_KIND = "notes_document";
const DOCUMENT_FORMAT = "sona-share-document-v1";
const DOCUMENT_INCLUDES = new Set(["notes", "notes_and_transcript", "everything"]);
const clockTime = /^\d{1,4}:\d{2}(?::\d{2})?$/u;

function utf8(value) {
  return encoder.encode(value);
}

function base64UrlDecode(value) {
  if (!/^[A-Za-z0-9_-]*$/u.test(value) || value.length % 4 === 1) return null;
  try {
    const padded =
      value.replaceAll("-", "+").replaceAll("_", "/") +
      "=".repeat((4 - (value.length % 4)) % 4);
    const binary = atob(padded);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1)
      bytes[index] = binary.charCodeAt(index);
    return bytes;
  } catch {
    return null;
  }
}

function base64UrlEncode(value) {
  let binary = "";
  for (let offset = 0; offset < value.length; offset += 0x8000) {
    binary += String.fromCharCode(
      ...value.subarray(offset, Math.min(offset + 0x8000, value.length)),
    );
  }
  return btoa(binary)
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

function u32(value) {
  const bytes = new Uint8Array(4);
  new DataView(bytes.buffer).setUint32(0, value, false);
  return bytes;
}

function concat(parts) {
  const length = parts.reduce((total, part) => total + part.length, 0);
  const output = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) {
    output.set(part, offset);
    offset += part.length;
  }
  return output;
}

function record(...values) {
  const fields = values.map((value) => {
    if (value instanceof Uint8Array) return value;
    return utf8(String(value));
  });
  const parts = [];
  for (const field of fields) parts.push(u32(field.length), field);
  return concat(parts);
}

function requireValue(condition) {
  if (!condition) throw new Error("invalid share bundle");
}
function isJsonObject(value) {
  return (
    !Array.isArray(value) &&
    !(value instanceof Function) &&
    Object(value) === value
  );
}

function isJsonString(value) {
  return (
    Object.prototype.toString.call(value) === "[object String]" &&
    !(value instanceof String)
  );
}

function opaqueId(value) {
  return isJsonString(value) && /^[A-Za-z0-9_-]{16,128}$/u.test(value);
}

function fixedBase64Url(value, byteLength) {
  if (!isJsonString(value)) return false;
  const bytes = base64UrlDecode(value);
  return (
    bytes !== null &&
    bytes.length === byteLength &&
    base64UrlEncode(bytes) === value
  );
}

function fixedDigest(value) {
  return fixedBase64Url(value, 32);
}

function fixedSignature(value) {
  return fixedBase64Url(value, 64);
}

async function shareKey(root, shareId, index, total, domain) {
  const inputKey = await crypto.subtle.importKey("raw", root, "HKDF", false, [
    "deriveKey",
  ]);
  return crypto.subtle.deriveKey(
    {
      name: "HKDF",
      hash: "SHA-256",
      salt: utf8("sona-share-v1"),
      info: record("sona-share-key-v1", shareId, index, total, domain),
    },
    inputKey,
    { name: "AES-GCM", length: 256 },
    false,
    ["decrypt"],
  );
}

export async function decryptPayload(
  root,
  shareId,
  index,
  total,
  domain,
  payload,
) {
  requireValue(root.length === 32 && payload.length >= 28);
  const key = await shareKey(root, shareId, index, total, domain);
  const decrypted = await crypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: payload.subarray(0, 12),
      additionalData: record(
        "sona-share-aad-v1",
        shareId,
        index,
        total,
        domain,
      ),
      tagLength: 128,
    },
    key,
    payload.subarray(12),
  );
  return new Uint8Array(decrypted);
}

export function parseFragment(hash) {
  const params = new URLSearchParams(
    hash.startsWith("#") ? hash.slice(1) : hash,
  );
  const seen = new Set();
  params.forEach((_value, key) => seen.add(key));
  requireValue(
    seen.size === 2 &&
      seen.has("v") &&
      seen.has("k") &&
      params.getAll("v").length === 1 &&
      params.getAll("k").length === 1,
  );
  requireValue(params.get("v") === "1");
  const root = base64UrlDecode(params.get("k"));
  requireValue(
    root !== null &&
      root.length === 32 &&
      base64UrlEncode(root) === params.get("k"),
  );
  return root;
}

function safeLink(value) {
  try {
    const url = new URL(value);
    if (
      url.protocol === "https:" ||
      url.protocol === "http:" ||
      url.protocol === "mailto:"
    )
      return url.href;
  } catch {
    return null;
  }
  return null;
}

function exactKeys(value, keys) {
  return (
    Object.keys(value).length === keys.length &&
    keys.every((key) => Object.prototype.hasOwnProperty.call(value, key))
  );
}

function assertRenderBounds(source) {
  let blocks = 1;
  let lineStart = 0;
  for (let index = 0; index < source.length; index += 1) {
    if (source[index] !== "\n") continue;
    requireValue(index - lineStart <= MAX_RENDER_LINE_CHARS);
    blocks += 1;
    requireValue(blocks <= MAX_RENDER_BLOCKS);
    lineStart = index + 1;
  }
  requireValue(source.length - lineStart <= MAX_RENDER_LINE_CHARS);
  let inlineTokens = 0;
  for (const _match of source.matchAll(inlineToken)) {
    inlineTokens += 1;
    requireValue(inlineTokens <= MAX_RENDER_INLINE_TOKENS);
  }
}

function appendInline(parent, source) {
  let cursor = 0;
  for (const match of source.matchAll(inlineToken)) {
    const start = match.index ?? cursor;
    if (start > cursor)
      parent.append(document.createTextNode(source.slice(cursor, start)));
    if (match[1] !== undefined) {
      const strong = document.createElement("strong");
      strong.textContent = match[1];
      parent.append(strong);
    } else if (match[2] !== undefined) {
      const code = document.createElement("code");
      code.textContent = match[2];
      parent.append(code);
    } else {
      const href = safeLink(match[4] ?? "");
      if (href === null) {
        parent.append(document.createTextNode(match[0]));
      } else {
        const anchor = document.createElement("a");
        anchor.href = href;
        anchor.rel = "noreferrer noopener";
        anchor.target = "_blank";
        anchor.textContent = match[3] ?? "";
        parent.append(anchor);
      }
    }
    cursor = start + match[0].length;
  }
  if (cursor < source.length)
    parent.append(document.createTextNode(source.slice(cursor)));
}

export function renderMarkdown(container, source) {
  const normalized = source.replaceAll("\r\n", "\n").replaceAll("\r", "\n");
  assertRenderBounds(normalized);
  container.replaceChildren();
  const lines = normalized.split("\n");
  let paragraph = [];
  let list = null;
  let codeLines = null;

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    const element = document.createElement("p");
    appendInline(element, paragraph.join("\n"));
    container.append(element);
    paragraph = [];
  };
  const flushList = () => {
    if (list !== null) container.append(list);
    list = null;
  };

  for (const line of lines) {
    if (codeLines !== null) {
      if (line.startsWith("```")) {
        const pre = document.createElement("pre");
        const code = document.createElement("code");
        code.textContent = codeLines.join("\n");
        pre.append(code);
        container.append(pre);
        codeLines = null;
      } else {
        codeLines.push(line);
      }
      continue;
    }
    if (line.startsWith("```")) {
      flushParagraph();
      flushList();
      codeLines = [];
      continue;
    }
    const heading = /^(#{1,3})\s+(.+)$/u.exec(line);
    if (heading !== null) {
      flushParagraph();
      flushList();
      const level = heading[1].length + 1;
      const element = document.createElement(`h${level}`);
      appendInline(element, heading[2]);
      container.append(element);
      continue;
    }
    if (/^[-*]\s+.+$/u.test(line)) {
      flushParagraph();
      if (list === null) list = document.createElement("ul");
      const item = document.createElement("li");
      appendInline(item, line.slice(2));
      list.append(item);
      continue;
    }
    if (line.startsWith("> ")) {
      flushParagraph();
      flushList();
      const quote = document.createElement("blockquote");
      appendInline(quote, line.slice(2));
      container.append(quote);
      continue;
    }
    if (line.trim().length === 0) {
      flushParagraph();
      flushList();
      continue;
    }
    flushList();
    paragraph.push(line);
  }
  if (codeLines !== null) {
    const pre = document.createElement("pre");
    const code = document.createElement("code");
    code.textContent = codeLines.join("\n");
    pre.append(code);
    container.append(pre);
  }
  flushParagraph();
  flushList();
}

function boundedText(value, max) {
  return isJsonString(value) && value.length > 0 && value.length <= max;
}

function parseBlock(block) {
  requireValue(isJsonObject(block));
  if (block.type === "paragraph") {
    requireValue(
      exactKeys(block, ["type", "text"]) &&
        boundedText(block.text, MAX_RENDER_LINE_CHARS),
    );
    return { type: "paragraph", text: block.text };
  }
  requireValue(
    block.type === "item" &&
      exactKeys(block, ["type", "text", "meta", "time"]) &&
      boundedText(block.text, MAX_RENDER_LINE_CHARS) &&
      (block.meta === null || boundedText(block.meta, MAX_RENDER_LINE_CHARS)) &&
      (block.time === null ||
        (isJsonString(block.time) && clockTime.test(block.time))),
  );
  return { type: "item", text: block.text, meta: block.meta, time: block.time };
}

export function parseShareDocument(source) {
  const value = JSON.parse(source);
  requireValue(isJsonObject(value));
  requireValue(
    exactKeys(value, [
      "version",
      "title",
      "include",
      "notes_out_of_date",
      "sections",
    ]),
  );
  requireValue(
    value.version === 1 &&
      boundedText(value.title, MAX_TITLE_CHARS) &&
      DOCUMENT_INCLUDES.has(value.include) &&
      (value.notes_out_of_date === true || value.notes_out_of_date === false) &&
      Array.isArray(value.sections) &&
      value.sections.length <= MAX_DOCUMENT_SECTIONS,
  );
  let blocks = 0;
  const sections = value.sections.map((section) => {
    requireValue(
      isJsonObject(section) && exactKeys(section, ["heading", "blocks"]),
    );
    requireValue(
      boundedText(section.heading, MAX_TITLE_CHARS) &&
        Array.isArray(section.blocks) &&
        section.blocks.length > 0,
    );
    blocks += section.blocks.length;
    requireValue(blocks <= MAX_RENDER_BLOCKS);
    return { heading: section.heading, blocks: section.blocks.map(parseBlock) };
  });
  return {
    title: value.title,
    include: value.include,
    notesOutOfDate: value.notes_out_of_date,
    sections,
  };
}

function shareNote(text) {
  const note = document.createElement("p");
  note.className = "share-note";
  note.textContent = text;
  return note;
}

function shareItem(block) {
  const item = document.createElement("li");
  const text = document.createElement("span");
  text.className = "share-text";
  text.textContent = block.text;
  const meta = document.createElement("span");
  meta.className = "share-meta";
  if (block.time !== null) {
    // A moment in the meeting: when, and who, above what was said.
    meta.textContent =
      block.meta === null ? block.time : `${block.time} · ${block.meta}`;
    item.append(meta, text);
  } else if (block.meta !== null) {
    meta.textContent = block.meta;
    item.append(text, meta);
  } else {
    item.append(text);
  }
  return item;
}

export function renderShareDocument(container, shared) {
  container.replaceChildren();
  if (shared.notesOutOfDate)
    container.append(
      shareNote("These notes were written before the transcript last changed."),
    );
  if (shared.sections.length === 0) {
    container.append(shareNote("Nothing was shared in this link."));
    return;
  }
  for (const section of shared.sections) {
    const element = document.createElement("section");
    const heading = document.createElement("h2");
    heading.textContent = section.heading;
    element.append(heading);
    let list = null;
    for (const block of section.blocks) {
      if (block.type === "paragraph") {
        list = null;
        const paragraph = document.createElement("p");
        paragraph.className = "share-paragraph";
        paragraph.textContent = block.text;
        element.append(paragraph);
        continue;
      }
      if (list === null) {
        list = document.createElement("ul");
        list.className = "share-items";
        element.append(list);
      }
      list.append(shareItem(block));
    }
    container.append(element);
  }
}

function parseManifestResponse(value) {
  requireValue(isJsonObject(value));
  const response = value;
  requireValue(exactKeys(response, ["version", "share", "manifest", "chunks"]));
  requireValue(
    response.version === 1 &&
      isJsonString(response.manifest) &&
      isJsonObject(response.share) &&
      Array.isArray(response.chunks),
  );
  const share = response.share;
  requireValue(
    exactKeys(share, [
      "share_id",
      "crypto_version",
      "manifest_sha256",
      "chunk_count",
      "total_bytes",
      "writer_signature",
    ]),
  );
  requireValue(
    opaqueId(share.share_id) &&
      share.crypto_version === 1 &&
      fixedDigest(share.manifest_sha256) &&
      fixedSignature(share.writer_signature),
  );
  requireValue(
    Number.isSafeInteger(share.chunk_count) &&
      share.chunk_count >= 0 &&
      share.chunk_count <= 4096,
  );
  requireValue(
    Number.isSafeInteger(share.total_bytes) &&
      share.total_bytes >= 0 &&
      share.total_bytes <= MAX_VIEWER_SHARE_CIPHERTEXT,
  );
  requireValue(response.chunks.length === share.chunk_count);
  const manifest = base64UrlDecode(response.manifest);
  requireValue(
    manifest !== null && base64UrlEncode(manifest) === response.manifest,
  );
  const chunks = response.chunks.map((candidate, index) => {
    requireValue(isJsonObject(candidate));
    requireValue(exactKeys(candidate, ["index", "size", "sha256"]));
    requireValue(
      candidate.index === index &&
        Number.isSafeInteger(candidate.size) &&
        candidate.size >= ENCRYPTED_PAYLOAD_OVERHEAD &&
        candidate.size <= 4 * 1024 * 1024 &&
        fixedDigest(candidate.sha256),
    );
    return { index, size: candidate.size, sha256: candidate.sha256 };
  });
  return { manifest, share, chunks };
}

function parseViewerManifest(bytes, chunkCount) {
  const value = JSON.parse(decoder.decode(bytes));
  requireValue(isJsonObject(value));
  requireValue(
    exactKeys(value, [
      "version",
      "kind",
      "source_format",
      "title",
      "chunk_count",
      "plaintext_bytes",
    ]),
  );
  requireValue(
    value.version === 1 &&
      ((value.kind === "markdown" && value.source_format === "markdown-utf8") ||
        (value.kind === DOCUMENT_KIND &&
          value.source_format === DOCUMENT_FORMAT)),
  );
  requireValue(
    isJsonString(value.title) && value.title.length <= MAX_TITLE_CHARS,
  );
  requireValue(
    value.chunk_count === chunkCount &&
      Number.isSafeInteger(value.plaintext_bytes) &&
      value.plaintext_bytes >= 0 &&
      value.plaintext_bytes <= MAX_VIEWER_PLAINTEXT,
  );
  return value;
}

async function digestMatches(bytes, expected) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return base64UrlEncode(digest) === expected;
}
async function fetchPlaintextChunk(root, shareId, chunkCount, chunk) {
  const response = await fetch(
    `/v1/shares/${encodeURIComponent(shareId)}/chunks/${chunk.index}`,
    {
      cache: "no-store",
      credentials: "omit",
      referrerPolicy: "no-referrer",
    },
  );
  if (!response.ok) throw new Error("share chunk unavailable");
  const ciphertext = new Uint8Array(await response.arrayBuffer());
  try {
    requireValue(
      ciphertext.length === chunk.size &&
        (await digestMatches(ciphertext, chunk.sha256)),
    );
    return await decryptPayload(
      root,
      shareId,
      chunk.index,
      chunkCount,
      "chunk",
      ciphertext,
    );
  } finally {
    ciphertext.fill(0);
  }
}

async function fetchPlaintextChunks(root, shareId, chunks) {
  let maximumPlaintextBytes = 0;
  for (const chunk of chunks) {
    maximumPlaintextBytes += chunk.size - ENCRYPTED_PAYLOAD_OVERHEAD;
    requireValue(maximumPlaintextBytes <= MAX_VIEWER_PLAINTEXT);
  }
  const plaintextChunks = chunks.map(() => undefined);
  let nextIndex = 0;
  let complete = false;
  const worker = async () => {
    const index = nextIndex;
    nextIndex += 1;
    if (index >= chunks.length) return;
    plaintextChunks[index] = await fetchPlaintextChunk(
      root,
      shareId,
      chunks.length,
      chunks[index],
    );
    return worker();
  };
  try {
    const settled = await Promise.allSettled(
      Array.from(
        { length: Math.min(MAX_CONCURRENT_CHUNK_REQUESTS, chunks.length) },
        () => worker(),
      ),
    );
    for (const result of settled) {
      if (result.status === "rejected") throw result.reason;
    }
    complete = true;
    return plaintextChunks;
  } finally {
    if (!complete) {
      for (const plaintext of plaintextChunks) {
        if (plaintext !== undefined) plaintext.fill(0);
      }
    }
  }
}

async function boot() {
  const title = document.getElementById("share-title");
  const status = document.getElementById("status");
  const content = document.getElementById("markdown-content");
  const download = document.getElementById("download-file");
  if (
    title === null ||
    status === null ||
    content === null ||
    download === null
  )
    return;
  const hash = location.hash;
  history.replaceState(null, "", `${location.pathname}${location.search}`);
  let root;
  try {
    root = parseFragment(hash);
    const parts = location.pathname.split("/");
    requireValue(parts.length === 3 && parts[1] === "s" && opaqueId(parts[2]));
    const shareId = parts[2];
    download.href = `/v1/shares/${encodeURIComponent(shareId)}/file`;
    const manifestResponse = await fetch(
      `/v1/shares/${encodeURIComponent(shareId)}/manifest`,
      {
        cache: "no-store",
        credentials: "omit",
        referrerPolicy: "no-referrer",
      },
    );
    if (!manifestResponse.ok) throw new Error("share manifest unavailable");
    const bundle = parseManifestResponse(await manifestResponse.json());
    requireValue(bundle.share.share_id === shareId);
    requireValue(
      await digestMatches(bundle.manifest, bundle.share.manifest_sha256),
    );
    const decryptedManifest = await decryptPayload(
      root,
      shareId,
      0,
      bundle.chunks.length,
      "manifest",
      bundle.manifest,
    );
    let viewerManifest;
    try {
      viewerManifest = parseViewerManifest(
        decryptedManifest,
        bundle.chunks.length,
      );
    } finally {
      decryptedManifest.fill(0);
    }
    const plaintextChunks = await fetchPlaintextChunks(
      root,
      shareId,
      bundle.chunks,
    );
    try {
      const plaintextLength = plaintextChunks.reduce(
        (total, plaintext) => total + plaintext.length,
        0,
      );
      requireValue(plaintextLength <= MAX_VIEWER_PLAINTEXT);
      requireValue(plaintextLength === viewerManifest.plaintext_bytes);
      const plaintext = concat(plaintextChunks);
      let text;
      try {
        text = decoder.decode(plaintext);
      } finally {
        plaintext.fill(0);
      }
      if (viewerManifest.kind === DOCUMENT_KIND) {
        const shared = parseShareDocument(text);
        title.textContent = shared.title;
        renderShareDocument(content, shared);
      } else {
        title.textContent = viewerManifest.title || "Sona shared note";
        renderMarkdown(content, text);
      }
      content.hidden = false;
      status.textContent = "Decrypted in this browser.";
    } finally {
      for (const plaintext of plaintextChunks) plaintext.fill(0);
    }
  } catch {
    title.textContent = "Share unavailable";
    status.textContent = "This share cannot be opened.";
    content.replaceChildren();
    content.hidden = true;
    download.removeAttribute("href");
  } finally {
    if (root instanceof Uint8Array) root.fill(0);
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", () => void boot(), {
    once: true,
  });
} else {
  void boot();
}
