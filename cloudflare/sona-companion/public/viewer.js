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
const DOCUMENT_INCLUDES = new Set([
  "notes",
  "notes_and_transcript",
  "everything",
]);
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

const LIVE_SALT = "sona-live-v1";
const LIVE_NAME_KEY = "sona-live-name";
const LIVE_EDIT_WAIT_MS = 20_000;
const LIVE_RETRY_LIMIT_MS = 30_000;
const LIVE_UNREACHABLE_AFTER = 3;
// The companion closes a socket with this once its room is gone
// (`src/constants.ts`).
const LIVE_ROOM_GONE = 4404;
// The Mac core drops a teammate's edit past these bounds (`live_share.rs`,
// `live_doc.rs`, `live_card.rs`).
const MAX_LIVE_NAME_CHARS = 40;
const RESERVED_LIVE_NAMES = new Set(["sona", "host", "you"]);
const MAX_LIVE_POINT_CHARS = 280;
const MAX_LIVE_CARD_CHARS = 120;
const MAX_LIVE_POINTS = 40;
const liveAuthors = new Set(["sona", "owner", "teammate"]);
const liveTargets = new Set(["point", "card"]);
const liveVerbs = new Map([
  ["added", "added"],
  ["rewrote", "rewrote"],
  ["edited", "changed"],
  ["removed", "removed"],
]);
const liveChangeKeys = [
  "id",
  "by",
  "at_ms",
  "call_ns",
  "target",
  "target_id",
  "kind",
  "text",
];
const liveDay = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  month: "short",
  day: "numeric",
});
const liveListNodes = new WeakMap();

function isFlag(value) {
  return value === true || value === false;
}

function isCount(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function storageArea(name) {
  return name === "session" ? sessionStorage : localStorage;
}

function stored(name, key) {
  try {
    return storageArea(name).getItem(key);
  } catch {
    return null;
  }
}

function keep(name, key, value) {
  try {
    storageArea(name).setItem(key, value);
  } catch {
    return;
  }
}

function liveRoot(roomId) {
  const key = `sona-live-root:${roomId}`;
  const hash = location.hash;
  if (hash.length > 1) {
    history.replaceState(null, "", `${location.pathname}${location.search}`);
    const root = parseFragment(hash);
    keep("session", key, base64UrlEncode(root));
    return root;
  }
  const kept = stored("session", key);
  requireValue(kept !== null);
  return parseFragment(`v=1&k=${kept}`);
}

async function liveSecrets(root, roomId) {
  const input = await crypto.subtle.importKey("raw", root, "HKDF", false, [
    "deriveBits",
    "deriveKey",
  ]);
  const salt = utf8(LIVE_SALT);
  const token = await crypto.subtle.deriveBits(
    {
      name: "HKDF",
      hash: "SHA-256",
      salt,
      info: record("sona-live-join-v1", roomId),
    },
    input,
    256,
  );
  const key = await crypto.subtle.deriveKey(
    {
      name: "HKDF",
      hash: "SHA-256",
      salt,
      info: record("sona-live-key-v1", roomId),
    },
    input,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
  return { joinToken: base64UrlEncode(new Uint8Array(token)), key };
}

function liveAad(roomId, kind, seq) {
  return record("sona-live-aad-v1", roomId, kind, seq);
}

async function openLiveBox(key, roomId, kind, seq, box) {
  const bytes = base64UrlDecode(box);
  requireValue(bytes !== null && bytes.length >= ENCRYPTED_PAYLOAD_OVERHEAD);
  const plaintext = await crypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: bytes.subarray(0, 12),
      additionalData: liveAad(roomId, kind, seq),
      tagLength: 128,
    },
    key,
    bytes.subarray(12),
  );
  return decoder.decode(plaintext);
}

async function sealLiveBox(key, roomId, kind, seq, text) {
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const sealed = await crypto.subtle.encrypt(
    {
      name: "AES-GCM",
      iv: nonce,
      additionalData: liveAad(roomId, kind, seq),
      tagLength: 128,
    },
    key,
    utf8(text),
  );
  return base64UrlEncode(concat([nonce, new Uint8Array(sealed)]));
}

function parseLiveFrame(data) {
  if (!isJsonString(data)) return null;
  let value;
  try {
    value = JSON.parse(data);
  } catch {
    return null;
  }
  if (!isJsonObject(value)) return null;
  if (
    value.type === "presence" &&
    exactKeys(value, ["type", "host"]) &&
    isFlag(value.host)
  )
    return value;
  if (value.type === "ended" && exactKeys(value, ["type"])) return value;
  if (
    value.type === "state" &&
    exactKeys(value, ["type", "seq", "box", "final"]) &&
    Number.isSafeInteger(value.seq) &&
    value.seq >= 1 &&
    isJsonString(value.box) &&
    isFlag(value.final)
  )
    return value;
  return null;
}

function parseLivePoint(point) {
  requireValue(
    isJsonObject(point) &&
      exactKeys(point, ["id", "text", "said_ns", "edited"]) &&
      boundedText(point.id, 128) &&
      isJsonString(point.text) &&
      (point.said_ns === null || isCount(point.said_ns)) &&
      isFlag(point.edited),
  );
  return { id: point.id, text: point.text };
}

function parseLiveCard(card) {
  requireValue(
    isJsonObject(card) &&
      exactKeys(card, [
        "id",
        "what",
        "owner",
        "due_at_ms",
        "due_words",
        "said_ns",
        "edited",
      ]) &&
      boundedText(card.id, 128) &&
      isJsonString(card.what) &&
      (card.owner === null || isJsonString(card.owner)) &&
      (card.due_at_ms === null || Number.isSafeInteger(card.due_at_ms)) &&
      (card.due_words === null || isJsonString(card.due_words)) &&
      isCount(card.said_ns) &&
      isFlag(card.edited),
  );
  return {
    id: card.id,
    what: card.what,
    owner: card.owner,
    dueAtMs: card.due_at_ms,
  };
}

function parseLiveLine(line) {
  requireValue(
    isJsonObject(line) &&
      exactKeys(line, ["said_ns", "who", "text"]) &&
      isCount(line.said_ns) &&
      (line.who === "You" || line.who === "Call") &&
      isJsonString(line.text),
  );
  return {
    saidNs: line.said_ns,
    who: line.who === "You" ? "Host" : "Call",
    text: line.text,
  };
}

function parseLiveChange(change) {
  requireValue(
    isJsonObject(change) &&
      (exactKeys(change, liveChangeKeys) ||
        exactKeys(change, [...liveChangeKeys, "name"])),
  );
  requireValue(
    boundedText(change.id, 128) &&
      liveAuthors.has(change.by) &&
      (change.name === undefined ||
        change.name === null ||
        isJsonString(change.name)) &&
      Number.isSafeInteger(change.at_ms) &&
      (change.call_ns === null || isCount(change.call_ns)) &&
      liveTargets.has(change.target) &&
      boundedText(change.target_id, 128) &&
      liveVerbs.has(change.kind) &&
      isJsonString(change.text),
  );
  return {
    id: change.id,
    by: change.by,
    name: change.name ?? null,
    atMs: change.at_ms,
    callNs: change.call_ns,
    target: change.target,
    targetId: change.target_id,
    kind: change.kind,
    text: change.text,
  };
}

function parseLiveState(source) {
  const value = JSON.parse(source);
  if (isJsonObject(value) && exactKeys(value, ["v", "removed"])) {
    requireValue(value.v === 1 && value.removed === true);
    return {
      removed: true,
      title: "",
      live: false,
      heardNs: 0,
      points: [],
      cards: [],
      lines: [],
      changes: [],
    };
  }
  requireValue(
    isJsonObject(value) &&
      exactKeys(value, [
        "v",
        "title",
        "live",
        "heard_ns",
        "points",
        "cards",
        "lines",
        "changes",
      ]),
  );
  requireValue(
    value.v === 1 &&
      isJsonString(value.title) &&
      isFlag(value.live) &&
      isCount(value.heard_ns) &&
      Array.isArray(value.points) &&
      Array.isArray(value.cards) &&
      Array.isArray(value.lines) &&
      Array.isArray(value.changes),
  );
  return {
    removed: false,
    title: value.title,
    live: value.live,
    heardNs: value.heard_ns,
    points: value.points.map(parseLivePoint),
    cards: value.cards.map(parseLiveCard),
    lines: value.lines.map(parseLiveLine),
    changes: value.changes.map(parseLiveChange),
  };
}

function liveText(value) {
  return value
    .replace(/\p{Cc}/gu, " ")
    .trim()
    .replace(/\s+/gu, " ");
}

function liveName(value) {
  const name = liveText(value);
  return name !== "" &&
    Array.from(name).length <= MAX_LIVE_NAME_CHARS &&
    !RESERVED_LIVE_NAMES.has(name.toLowerCase())
    ? name
    : null;
}

function liveClock(ns) {
  const total = Math.floor(ns / 1_000_000_000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = String(total % 60).padStart(2, "0");
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}`
    : `${minutes}:${seconds}`;
}

function changeAuthor(change) {
  if (change.by === "sona") return "Sona";
  if (change.by === "owner") return "Host";
  return change.name ?? "A teammate";
}

function changeWhen(change) {
  return change.callNs === null ? "After the call" : liveClock(change.callNs);
}

function changeMark(change) {
  if (change === undefined) return "";
  return `${changeAuthor(change)} · ${changeWhen(change)}`;
}

function liveTimeline(lines, changes) {
  const pending = [...changes].sort(
    (a, b) =>
      (a.callNs ?? Infinity) - (b.callNs ?? Infinity) || a.atMs - b.atMs,
  );
  const entries = [];
  const repeats = new Map();
  let next = 0;
  for (const line of lines) {
    while (
      next < pending.length &&
      pending[next].callNs !== null &&
      pending[next].callNs < line.saidNs
    ) {
      entries.push({
        key: `change:${pending[next].id}`,
        change: pending[next],
      });
      next += 1;
    }
    const base = `line:${line.saidNs}:${line.who}`;
    const repeat = repeats.get(base) ?? 0;
    repeats.set(base, repeat + 1);
    entries.push({ key: `${base}:${repeat}`, line });
  }
  for (const change of pending.slice(next))
    entries.push({ key: `change:${change.id}`, change });
  return entries;
}

function setText(element, value) {
  if (element.textContent !== value) element.textContent = value;
}

function showValue(grow, value) {
  const field = grow.firstElementChild;
  if (field.value !== value) field.value = value;
  if (grow.dataset.value !== value) grow.dataset.value = value;
}

function syncLiveList(list, items, render) {
  const previous = liveListNodes.get(list) ?? new Map();
  const next = new Map();
  for (const item of items)
    next.set(item.key, render(item, previous.get(item.key)));
  for (const [key, node] of previous) if (next.get(key) !== node) node.remove();
  let cursor = list.firstElementChild;
  for (const node of next.values()) {
    if (node === cursor) cursor = cursor.nextElementSibling;
    else list.insertBefore(node, cursor);
  }
  liveListNodes.set(list, next);
}

function liveEditable(page) {
  return (
    page.name !== null &&
    page.open &&
    page.hostPresent === true &&
    page.state !== null &&
    page.state.live &&
    !page.ended
  );
}

function liveStatus(page) {
  if (page.state?.removed) return "The host removed this doc.";
  if (page.gone) return "This link has expired.";
  if (page.ended) return "The call ended. This doc is read-only.";
  if (page.state !== null && !page.state.live)
    return "The call ended. Sona is finishing the doc.";
  if (!page.open) {
    if (page.attempts >= LIVE_UNREACHABLE_AFTER)
      return "Cannot reach this live doc. Retrying";
    return page.opened ? "Reconnecting" : "Connecting";
  }
  if (page.hostPresent === null) return "Connecting";
  if (!page.hostPresent)
    return page.state === null
      ? "Waiting for the host"
      : "The host is offline. Edits are paused.";
  return page.state === null ? "Waiting for the doc" : "Live";
}

function pendingLiveEdit(page, key) {
  let found;
  for (const edit of page.edits.values()) if (edit.key === key) found = edit;
  return found;
}

function settleLiveEdit(page, id, landed) {
  const edit = page.edits.get(id) ?? page.late.get(id);
  if (edit === undefined) return;
  clearTimeout(edit.timer);
  page.edits.delete(id);
  page.late.delete(id);
  const added = page.adds.get(edit.key);
  if (added !== undefined) {
    if (landed) page.adds.delete(edit.key);
    else added.note = "Not saved";
    return;
  }
  if (!landed) {
    page.notes.set(edit.key, "Not saved");
    return;
  }
  const draft = page.drafts.get(edit.key);
  if (draft !== undefined && liveText(draft) === edit.text)
    page.drafts.delete(edit.key);
  if (pendingLiveEdit(page, edit.key) === undefined)
    page.notes.delete(edit.key);
}

async function sendLiveEdit(page, target, targetId, text) {
  const id = base64UrlEncode(crypto.getRandomValues(new Uint8Array(16)));
  const key = targetId === null ? `add:${id}` : `${target}:${targetId}`;
  page.editSeq += 1;
  const seq = page.editSeq;
  const edit = {
    key,
    text,
    timer: setTimeout(() => {
      settleLiveEdit(page, id, false);
      page.late.set(id, edit);
      renderLive(page);
    }, LIVE_EDIT_WAIT_MS),
  };
  page.edits.set(id, edit);
  if (targetId === null) page.adds.set(key, { text, note: "Sending" });
  else page.notes.set(key, text === "" ? "Removing" : "Sending");
  renderLive(page);
  const box = await sealLiveBox(
    page.key,
    page.roomId,
    "edit",
    seq,
    JSON.stringify({
      v: 1,
      id,
      name: page.name,
      target,
      target_id: targetId,
      text,
    }),
  );
  if (page.edits.get(id) !== edit) return;
  if (!page.open) {
    settleLiveEdit(page, id, false);
    renderLive(page);
    return;
  }
  page.socket.send(JSON.stringify({ type: "edit", seq, box }));
}

function liveItemText(page, target, id) {
  if (target === "point")
    return page.state.points.find((point) => point.id === id)?.text ?? null;
  return page.state.cards.find((card) => card.id === id)?.what ?? null;
}

function commitLiveField(page, target, id) {
  const key = `${target}:${id}`;
  const draft = page.drafts.get(key);
  if (draft === undefined) return;
  const text = liveText(draft);
  const current = liveItemText(page, target, id);
  const pending = pendingLiveEdit(page, key);
  if (
    current === null ||
    text === "" ||
    (pending === undefined && text === current)
  ) {
    page.drafts.delete(key);
    renderLive(page);
    return;
  }
  if (pending?.text === text) return;
  if (!liveEditable(page)) {
    page.notes.set(key, "Not saved");
    renderLive(page);
    return;
  }
  void sendLiveEdit(page, target, id, text);
}

function addLivePoint(page) {
  const { addInput } = page.elements;
  const text = liveText(addInput.value);
  if (
    text === "" ||
    !liveEditable(page) ||
    page.state.points.length >= MAX_LIVE_POINTS
  )
    return;
  showValue(addInput.parentElement, "");
  void sendLiveEdit(page, "point", null, text);
}

function removeLivePoint(page, id) {
  const key = `point:${id}`;
  if (!liveEditable(page) || pendingLiveEdit(page, key)?.text === "") return;
  page.drafts.delete(key);
  void sendLiveEdit(page, "point", id, "");
}

function liveGrow(label, max) {
  const grow = document.createElement("div");
  grow.className = "live-grow";
  const field = document.createElement("textarea");
  field.className = "live-field";
  field.rows = 1;
  field.maxLength = max;
  field.setAttribute("aria-label", label);
  field.setAttribute("enterkeyhint", "done");
  grow.append(field);
  return grow;
}

function liveMark() {
  const mark = document.createElement("span");
  mark.className = "live-mark";
  return mark;
}

function bindLiveField(page, grow, target, id) {
  const field = grow.firstElementChild;
  const key = `${target}:${id}`;
  field.addEventListener("input", () => {
    page.drafts.set(key, field.value);
    grow.dataset.value = field.value;
    if (page.notes.get(key) === "Not saved") {
      page.notes.delete(key);
      renderLive(page);
    }
  });
  field.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.isComposing) {
      event.preventDefault();
      commitLiveField(page, target, id);
    } else if (event.key === "Escape") {
      page.drafts.delete(key);
      renderLive(page);
    }
  });
  field.addEventListener("blur", () => commitLiveField(page, target, id));
}

function pointRow(page, point, node, marks, editable) {
  const key = `point:${point.id}`;
  let row = node;
  if (row === undefined) {
    row = document.createElement("li");
    row.className = "live-item";
    const grow = liveGrow("Point", MAX_LIVE_POINT_CHARS);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "live-remove";
    remove.textContent = "Remove";
    remove.setAttribute("aria-label", "Remove point");
    remove.addEventListener("click", () => removeLivePoint(page, point.id));
    bindLiveField(page, grow, "point", point.id);
    row.append(grow, liveMark(), remove);
  }
  const [grow, mark, remove] = row.children;
  showValue(grow, page.drafts.get(key) ?? point.text);
  grow.firstElementChild.readOnly = !editable;
  setText(mark, page.notes.get(key) ?? changeMark(marks.get(point.id)));
  remove.hidden = !editable;
  return row;
}

function addedRow(added, node) {
  let row = node;
  if (row === undefined) {
    row = document.createElement("li");
    row.className = "live-item live-added";
    const text = document.createElement("span");
    text.className = "live-added-text";
    row.append(text, liveMark());
  }
  const [text, mark] = row.children;
  setText(text, added.text);
  setText(mark, added.note);
  return row;
}

function cardMeta(card) {
  const owner = card.owner ?? "No owner";
  return card.dueAtMs === null
    ? owner
    : `${owner} · ${liveDay.format(card.dueAtMs)}`;
}

function cardRow(page, card, node, marks, editable) {
  const key = `card:${card.id}`;
  let row = node;
  if (row === undefined) {
    row = document.createElement("li");
    row.className = "live-item";
    const grow = liveGrow("What is owed", MAX_LIVE_CARD_CHARS);
    const meta = document.createElement("span");
    meta.className = "live-card-meta";
    bindLiveField(page, grow, "card", card.id);
    row.append(grow, liveMark(), meta);
  }
  const [grow, mark, meta] = row.children;
  showValue(grow, page.drafts.get(key) ?? card.what);
  grow.firstElementChild.readOnly = !editable;
  setText(mark, page.notes.get(key) ?? changeMark(marks.get(card.id)));
  setText(meta, cardMeta(card));
  return row;
}

function timelineRow(entry, node) {
  let row = node;
  if (row === undefined) {
    row = document.createElement("li");
    row.className =
      entry.line === undefined ? "live-entry live-entry-change" : "live-entry";
    const meta = document.createElement("span");
    meta.className = "live-entry-meta";
    const text = document.createElement("span");
    text.className = "live-entry-text";
    row.append(meta, text);
  }
  const [meta, text] = row.children;
  if (entry.line === undefined) {
    const { change } = entry;
    setText(
      meta,
      `${changeWhen(change)} · ${changeAuthor(change)} ${liveVerbs.get(change.kind)} a ${change.target}`,
    );
    setText(text, change.text);
  } else {
    setText(meta, `${liveClock(entry.line.saidNs)} · ${entry.line.who}`);
    setText(text, entry.line.text);
  }
  return row;
}

function renderLiveTimeline(page, closed) {
  const { timelineList, timelineEmpty } = page.elements;
  const entries = liveTimeline(page.state.lines, page.state.changes);
  const atEnd =
    timelineList.scrollHeight -
      timelineList.scrollTop -
      timelineList.clientHeight <
    8;
  syncLiveList(timelineList, entries, timelineRow);
  if (atEnd) timelineList.scrollTop = timelineList.scrollHeight;
  timelineEmpty.hidden = entries.length > 0;
  setText(
    timelineEmpty,
    closed ? "Nothing was heard" : "Lines show here as they are heard.",
  );
}

function renderLive(page) {
  const { elements, state } = page;
  const closed = page.ended || page.gone || (state !== null && !state.live);
  const editable = liveEditable(page);
  const status = liveStatus(page);
  const title = state === null || state.title === "" ? "Live doc" : state.title;
  setText(elements.title, title);
  document.title = `${title} — Sona`;
  setText(elements.status, status);
  setText(
    elements.clock,
    status === "Live" ? ` · ${liveClock(state.heardNs)} into the call` : "",
  );
  const asking =
    state !== null && !closed && (page.name === null || page.renaming);
  elements.nameForm.hidden = !asking;
  elements.editor.hidden = state === null || closed || asking;
  setText(elements.editorName, `Editing as ${page.name ?? ""}`);
  const removed = state?.removed === true;
  elements.layout.hidden = state === null || removed;
  elements.timeline.hidden = state === null || removed;
  if (state === null || removed) return;

  const marks = new Map(
    state.changes.map((change) => [change.targetId, change]),
  );
  const points = state.points.map((point) => ({
    key: `point:${point.id}`,
    point,
  }));
  for (const [key, added] of page.adds) points.push({ key, added });
  syncLiveList(elements.points, points, (item, node) =>
    item.point === undefined
      ? addedRow(item.added, node)
      : pointRow(page, item.point, node, marks, editable),
  );
  elements.pointsEmpty.hidden = points.length > 0;
  setText(
    elements.pointsEmpty,
    closed ? "No points" : "Points show here as they are said.",
  );
  const full = state.points.length >= MAX_LIVE_POINTS;
  elements.addForm.hidden = !editable;
  elements.addFields.disabled = full;
  setText(elements.addNote, full ? "The doc holds up to 40 points" : "");

  const cards = state.cards.map((card) => ({ key: `card:${card.id}`, card }));
  syncLiveList(elements.cards, cards, (item, node) =>
    cardRow(page, item.card, node, marks, editable),
  );
  elements.cardsEmpty.hidden = cards.length > 0;
  setText(
    elements.cardsEmpty,
    closed
      ? "Nothing on the board"
      : "Who owes what by when shows here as it is said.",
  );
  renderLiveTimeline(page, closed);
}

async function applyLiveState(page, frame) {
  if (frame.seq <= page.seq) return;
  let state;
  try {
    state = parseLiveState(
      await openLiveBox(page.key, page.roomId, "state", frame.seq, frame.box),
    );
  } catch {
    return;
  }
  page.seq = frame.seq;
  page.state = state;
  if (frame.final) page.ended = true;
  const landed = new Set(state.changes.map((change) => change.id));
  const closing = frame.final || !state.live;
  for (const id of [...page.edits.keys(), ...page.late.keys()]) {
    if (landed.has(id)) settleLiveEdit(page, id, true);
    else if (closing) settleLiveEdit(page, id, false);
  }
  if (closing) page.drafts.clear();
  renderLive(page);
}

async function drainLiveStates(page) {
  if (page.draining) return;
  page.draining = true;
  try {
    while (page.nextState !== null) {
      const frame = page.nextState;
      page.nextState = null;
      await applyLiveState(page, frame);
    }
  } finally {
    page.draining = false;
  }
}

function receiveLiveFrame(page, frame) {
  if (frame === null) return;
  if (frame.type === "presence") {
    page.hostPresent = frame.host;
    renderLive(page);
    return;
  }
  if (frame.type === "ended") {
    page.ended = true;
    for (const id of page.edits.keys()) settleLiveEdit(page, id, false);
    page.late.clear();
    page.drafts.clear();
    renderLive(page);
    return;
  }
  if (page.nextState === null || frame.seq > page.nextState.seq)
    page.nextState = frame;
  void drainLiveStates(page);
}

function connectLive(page) {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const socket = new WebSocket(
    `${scheme}://${location.host}/v1/live/${page.roomId}/socket?t=${page.joinToken}`,
  );
  page.socket = socket;
  socket.addEventListener("open", () => {
    page.open = true;
    page.opened = true;
    page.attempts = 0;
    renderLive(page);
  });
  socket.addEventListener("message", (event) =>
    receiveLiveFrame(page, parseLiveFrame(event.data)),
  );
  socket.addEventListener("close", (event) => {
    page.open = false;
    page.hostPresent = null;
    if (event.code === LIVE_ROOM_GONE) {
      page.gone = true;
      renderLive(page);
      return;
    }
    page.attempts += 1;
    const delay = Math.min(
      LIVE_RETRY_LIMIT_MS,
      1000 * 2 ** (page.attempts - 1),
    );
    setTimeout(
      () => connectLive(page),
      delay / 2 + Math.random() * (delay / 2),
    );
    renderLive(page);
  });
}

function liveElements() {
  const elements = {
    title: document.getElementById("live-title"),
    status: document.getElementById("live-status"),
    clock: document.getElementById("live-clock"),
    nameForm: document.getElementById("live-name-form"),
    nameInput: document.getElementById("live-name-input"),
    nameHint: document.getElementById("live-name-hint"),
    editor: document.getElementById("live-editor"),
    editorName: document.getElementById("live-editor-name"),
    rename: document.getElementById("live-rename"),
    layout: document.getElementById("live-layout"),
    points: document.getElementById("live-points"),
    pointsEmpty: document.getElementById("live-points-empty"),
    addForm: document.getElementById("live-add"),
    addFields: document.getElementById("live-add-fields"),
    addInput: document.getElementById("live-add-input"),
    addNote: document.getElementById("live-add-note"),
    cards: document.getElementById("live-cards"),
    cardsEmpty: document.getElementById("live-cards-empty"),
    timeline: document.getElementById("live-timeline"),
    timelineList: document.getElementById("live-timeline-list"),
    timelineEmpty: document.getElementById("live-timeline-empty"),
  };
  return Object.values(elements).includes(null) ? null : elements;
}

function wireLive(page) {
  const { nameForm, nameInput, nameHint, rename, addForm, addInput } =
    page.elements;
  nameForm.addEventListener("submit", (event) => {
    event.preventDefault();
    const name = liveName(nameInput.value);
    if (name === null) {
      setText(nameHint, "Enter your own name to edit");
      return;
    }
    page.name = name;
    page.renaming = false;
    keep("local", LIVE_NAME_KEY, name);
    setText(nameHint, "Shown next to your edits");
    renderLive(page);
  });
  rename.addEventListener("click", () => {
    page.renaming = true;
    nameInput.value = page.name ?? "";
    renderLive(page);
    nameInput.focus();
  });
  addForm.addEventListener("submit", (event) => {
    event.preventDefault();
    addLivePoint(page);
  });
  addInput.addEventListener("input", () => {
    addInput.parentElement.dataset.value = addInput.value;
  });
  addInput.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" || event.isComposing) return;
    event.preventDefault();
    addLivePoint(page);
  });
}

async function bootLive() {
  const view = document.getElementById("live-view");
  const elements = liveElements();
  if (view === null || elements === null) return;
  view.hidden = false;
  const roomId = location.pathname.slice("/l/".length);
  let root;
  try {
    requireValue(opaqueId(roomId));
    root = liveRoot(roomId);
  } catch {
    setText(elements.title, "Live doc unavailable");
    setText(elements.status, "This link cannot be opened.");
    document.title = "Live doc unavailable — Sona";
    return;
  }
  let secrets;
  try {
    secrets = await liveSecrets(root, roomId);
  } finally {
    root.fill(0);
  }
  const page = {
    adds: new Map(),
    attempts: 0,
    draining: false,
    drafts: new Map(),
    editSeq: 0,
    edits: new Map(),
    elements,
    ended: false,
    gone: false,
    hostPresent: null,
    joinToken: secrets.joinToken,
    key: secrets.key,
    late: new Map(),
    name: liveName(stored("local", LIVE_NAME_KEY) ?? ""),
    nextState: null,
    notes: new Map(),
    open: false,
    opened: false,
    renaming: false,
    roomId,
    seq: 0,
    socket: null,
    state: null,
  };
  wireLive(page);
  renderLive(page);
  connectLive(page);
}

async function boot() {
  if (location.pathname.startsWith("/l/")) {
    await bootLive();
    return;
  }
  const shareView = document.getElementById("share-view");
  if (shareView !== null) shareView.hidden = false;
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
