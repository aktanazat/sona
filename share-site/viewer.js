const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });

const PAGE_FORMAT = "sona-meeting-page-v1";
const PAGE_KEYS = [
  "format",
  "title",
  "date",
  "duration_s",
  "summary",
  "decisions",
  "action_items",
  "notes",
  "speakers",
  "transcript",
  "audio",
  "shared_at",
  "expires_at",
];
const MAX_TITLE_CHARS = 240;
const MAX_TEXT_BYTES = 64 * 1024;
const MAX_TURNS = 20_000;
const MAX_PAGE_BYTES = 8 * 1024 * 1024;
const MAX_AUDIO_PARTS = 64;
const MAX_AUDIO_PART_BYTES = 24 * 1024 * 1024;
const ENCRYPTED_PAYLOAD_OVERHEAD = 12 + 16;
const AUDIO_FETCHES = 3;
const FIRST_TURNS = 200;
const TURN_BATCH = 1000;
const LONG_TRANSCRIPT = 400;
const rfc3339 =
  /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/u;
const slugPattern = /^[A-Za-z0-9_-]{22}$/u;
const audioType = /^audio\/[a-z0-9][a-z0-9.+-]{0,63}$/u;
const localFileName = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/u;
const fetchOptions = {
  cache: "no-store",
  credentials: "omit",
  referrerPolicy: "no-referrer",
};

const STATES = {
  missingKey: [
    "This link is missing its key",
    "Ask the person who shared it for the whole link, including the part after the #.",
  ],
  gone: [
    "This page was removed or has expired",
    "Ask the person who shared it for a new link.",
  ],
  wrongKey: [
    "This key does not open these notes",
    "The link may have been cut short. Copy the whole link and open it again.",
  ],
  unreadable: [
    "These notes could not be read",
    "Ask the person who shared them for a new link.",
  ],
  offline: [
    "The notes could not be loaded",
    "Check your connection and reload the page.",
  ],
};

const dayFormat = new Intl.DateTimeFormat("en-US", { dateStyle: "long" });
const momentFormat = new Intl.DateTimeFormat("en-US", {
  dateStyle: "long",
  timeStyle: "short",
});

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
  if (!condition) throw new Error("invalid shared page");
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

async function decryptPayload(root, shareId, index, total, domain, payload) {
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

function parseFragment(hash) {
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

function exactKeys(value, keys) {
  if (!isJsonObject(value)) return false;
  const own = Object.keys(value);
  return (
    own.length === keys.length &&
    keys.every((key) => Object.prototype.hasOwnProperty.call(value, key))
  );
}

function isText(value) {
  return (
    isJsonString(value) &&
    value.length > 0 &&
    (value.length * 3 <= MAX_TEXT_BYTES ||
      utf8(value).length <= MAX_TEXT_BYTES)
  );
}

function isMoment(value) {
  return (
    isJsonString(value) &&
    rfc3339.test(value) &&
    Number.isFinite(Date.parse(value))
  );
}

function isSeconds(value) {
  return Number.isFinite(value) && value >= 0;
}

function isTextList(value) {
  return Array.isArray(value) && value.length > 0 && value.every(isText);
}

function isActionItem(value) {
  return (
    exactKeys(value, ["text", "owner", "due"]) &&
    isText(value.text) &&
    (value.owner === null || isText(value.owner)) &&
    (value.due === null || isText(value.due))
  );
}

function isAudio(value) {
  return (
    exactKeys(value, ["type", "parts", "bytes", "duration_s"]) &&
    isJsonString(value.type) &&
    audioType.test(value.type) &&
    Number.isInteger(value.parts) &&
    value.parts >= 1 &&
    value.parts <= MAX_AUDIO_PARTS &&
    Number.isInteger(value.bytes) &&
    value.bytes > 0 &&
    value.bytes <= value.parts * MAX_AUDIO_PART_BYTES &&
    isSeconds(value.duration_s)
  );
}

function isTurn(value, speakerCount) {
  return (
    exactKeys(value, ["s", "t", "text"]) &&
    Number.isInteger(value.s) &&
    value.s >= 0 &&
    value.s < speakerCount &&
    isSeconds(value.t) &&
    isText(value.text)
  );
}

function parsePage(page) {
  requireValue(exactKeys(page, PAGE_KEYS) && page.format === PAGE_FORMAT);
  if (page.title === null) {
    requireValue(page.date === null && page.duration_s === null);
  } else {
    requireValue(
      isText(page.title) &&
        [...page.title].length <= MAX_TITLE_CHARS &&
        (page.date === null || isMoment(page.date)) &&
        (page.duration_s === null || isSeconds(page.duration_s)),
    );
  }
  for (const key of ["summary", "decisions", "notes"])
    requireValue(page[key] === null || isTextList(page[key]));
  requireValue(
    page.action_items === null ||
      (Array.isArray(page.action_items) &&
        page.action_items.length > 0 &&
        page.action_items.every(isActionItem)),
  );
  if (page.transcript === null) {
    requireValue(page.speakers === null);
  } else {
    requireValue(
      isTextList(page.speakers) &&
        Array.isArray(page.transcript) &&
        page.transcript.length > 0 &&
        page.transcript.length <= MAX_TURNS &&
        page.transcript.every((turn) => isTurn(turn, page.speakers.length)),
    );
  }
  requireValue(page.audio === null || isAudio(page.audio));
  requireValue(
    isMoment(page.shared_at) &&
      (page.expires_at === null || isMoment(page.expires_at)),
  );
  return page;
}

function node(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.append(document.createTextNode(text));
  return element;
}

function twoDigits(value) {
  return String(value).padStart(2, "0");
}

function clock(seconds) {
  const whole = Math.floor(seconds);
  const hours = Math.floor(whole / 3600);
  const minutes = Math.floor((whole % 3600) / 60);
  const rest = twoDigits(whole % 60);
  return hours > 0
    ? `${hours}:${twoDigits(minutes)}:${rest}`
    : `${minutes}:${rest}`;
}

function clockWidth(seconds) {
  if (seconds >= 36_000) return "hh";
  if (seconds >= 3600) return "h";
  if (seconds >= 600) return "mm";
  return "m";
}

function duration(seconds) {
  if (seconds < 60) return `${Math.max(1, Math.round(seconds))} sec`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours} hr` : `${hours} hr ${rest} min`;
}

const view = document.getElementById("view");
const recording = {
  slot: node("div", "recording"),
  audio: null,
  ready: false,
  started: false,
};
let generation = 0;
let transcript = null;

function showState([title, detail]) {
  generation += 1;
  transcript = null;
  if (recording.audio !== null) recording.audio.pause();
  document.title = title;
  view.replaceChildren(node("h1", null, title), node("p", null, detail));
}

function section(title, ...content) {
  const element = node("section");
  element.append(node("h2", null, title), ...content);
  return element;
}

function paragraphs(texts) {
  return texts.map((text) => node("p", null, text));
}

function list(items, render) {
  const element = node("ul");
  for (const item of items) element.append(render(item));
  return element;
}

function actionItem(item) {
  const element = node("li", null, item.text);
  const meta = [];
  if (item.owner !== null) meta.push(item.owner);
  if (item.due !== null)
    meta.push(meta.length === 0 ? `Due ${item.due}` : `due ${item.due}`);
  if (meta.length > 0) element.append(node("span", "owner", meta.join(", ")));
  return element;
}

function turnAt(view, seconds) {
  let low = 0;
  let high = view.times.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (view.times[middle] <= seconds) low = middle + 1;
    else high = middle;
  }
  return low === 0 ? -1 : view.order[low - 1];
}

function markPlaying() {
  if (transcript === null || !transcript.timed || !recording.started) return;
  const index = turnAt(transcript, recording.audio.currentTime);
  if (index === transcript.active) return;
  const previous = transcript.items[transcript.active];
  if (previous !== undefined) previous.removeAttribute("aria-current");
  transcript.active = index;
  const current = transcript.items[index];
  if (current !== undefined) current.setAttribute("aria-current", "true");
}

function setTimesEnabled(enabled) {
  if (transcript === null || !transcript.timed) return;
  for (const item of transcript.items) item.firstChild.disabled = !enabled;
}

function turnItem(page, index, timed) {
  const turn = page.transcript[index];
  const time = clock(turn.t);
  const item = node("li", "turn");
  let stamp;
  if (timed) {
    stamp = node("button", "time", time);
    stamp.type = "button";
    stamp.dataset.index = String(index);
    stamp.disabled = !recording.ready;
    stamp.setAttribute("aria-label", `Play from ${time}`);
  } else {
    stamp = node("span", "time", time);
  }
  const said = node("div", "said");
  said.append(
    node("span", "who", page.speakers[turn.s]),
    node("p", null, turn.text),
  );
  item.append(stamp, said);
  if (transcript !== null && index === transcript.active)
    item.setAttribute("aria-current", "true");
  return item;
}

function appendTurns(page, start, end) {
  const fragment = document.createDocumentFragment();
  for (let index = start; index < end; index += 1) {
    const item = turnItem(page, index, transcript.timed);
    transcript.items.push(item);
    fragment.append(item);
  }
  transcript.list.append(fragment);
}

async function fillTranscript(page, renderGeneration) {
  const total = page.transcript.length;
  for (let start = FIRST_TURNS; start < total; start += TURN_BATCH) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    if (renderGeneration !== generation) return;
    appendTurns(page, start, Math.min(total, start + TURN_BATCH));
  }
}

function transcriptSection(page) {
  const turns = page.transcript;
  const order = turns.map((_turn, index) => index);
  order.sort((a, b) => turns[a].t - turns[b].t || a - b);
  const times = Float64Array.from(order, (index) => turns[index].t);
  const listElement = node("ol", "transcript");
  listElement.dataset.clock = clockWidth(times[times.length - 1]);
  if (turns.length > LONG_TRANSCRIPT) listElement.dataset.long = "";
  transcript = {
    list: listElement,
    items: [],
    order,
    times,
    active: -1,
    timed: page.audio !== null,
  };
  listElement.addEventListener("click", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const button = target.closest("button.time");
    if (button === null || button.disabled || recording.audio === null) return;
    recording.audio.currentTime = turns[Number(button.dataset.index)].t;
    recording.audio.play().catch(() => {});
  });
  appendTurns(page, 0, Math.min(turns.length, FIRST_TURNS));
  return section("Transcript", listElement);
}

function footer(page) {
  const element = node("footer");
  let when = `Shared ${dayFormat.format(new Date(page.shared_at))}.`;
  if (page.expires_at !== null)
    when += ` This link stops working ${momentFormat.format(new Date(page.expires_at))}.`;
  const what =
    page.audio === null
      ? "The text is decrypted in this browser with the key at the end of the link."
      : "The text and recording are decrypted in this browser with the key at the end of the link.";
  element.append(node("p", null, when), node("p", null, what));
  return element;
}

function renderPage(page) {
  generation += 1;
  transcript = null;
  const title = page.title ?? "Meeting notes";
  document.title = title;
  const content = [node("h1", null, title)];
  const when = [];
  if (page.date !== null) when.push(momentFormat.format(new Date(page.date)));
  if (page.duration_s !== null) when.push(duration(page.duration_s));
  if (when.length > 0) {
    const line = node("p", "date");
    line.append(
      ...when.flatMap((text, index) => [
        ...(index > 0 ? [" · "] : []),
        node("span", "together", text),
      ]),
    );
    content.push(line);
  }
  if (page.audio !== null) content.push(recording.slot);
  if (page.summary !== null)
    content.push(section("Summary", ...paragraphs(page.summary)));
  if (page.decisions !== null)
    content.push(
      section(
        "Decisions",
        list(page.decisions, (text) => node("li", null, text)),
      ),
    );
  if (page.action_items !== null)
    content.push(section("Action items", list(page.action_items, actionItem)));
  if (page.notes !== null)
    content.push(section("Notes", ...paragraphs(page.notes)));
  if (page.transcript !== null) content.push(transcriptSection(page));
  content.push(footer(page));
  view.replaceChildren(...content);
  if (page.transcript !== null) void fillTranscript(page, generation);
  markPlaying();
}

function useAudio(audio) {
  recording.audio = audio;
  audio.controls = true;
  audio.preload = "metadata";
  audio.setAttribute("aria-label", "Recording");
  audio.addEventListener("timeupdate", markPlaying);
  audio.addEventListener("seeked", markPlaying);
  audio.addEventListener(
    "play",
    () => {
      recording.started = true;
      recording.slot.classList.add("following");
      markPlaying();
    },
    { once: true },
  );
  audio.addEventListener("error", () => recordingFailed());
  recording.ready = true;
  recording.slot.replaceChildren(audio);
  setTimesEnabled(true);
}

function recordingNote(text) {
  recording.ready = false;
  recording.slot.classList.remove("following");
  recording.slot.replaceChildren(node("p", "quiet", text));
  setTimesEnabled(false);
}

function recordingFailed() {
  recordingNote("The recording could not be loaded. The notes are complete.");
}

function nextPaint() {
  return new Promise((resolve) =>
    requestAnimationFrame(() => setTimeout(resolve, 0)),
  );
}

async function loadAudio(root, slug, directory, audio) {
  const parts = Array.from({ length: audio.parts });
  let next = 0;
  const worker = async () => {
    while (next < audio.parts) {
      const index = next;
      next += 1;
      const response = await fetch(
        new URL(`audio.${index}.sona`, directory),
        fetchOptions,
      );
      requireValue(response.ok);
      const payload = new Uint8Array(await response.arrayBuffer());
      requireValue(
        payload.length > ENCRYPTED_PAYLOAD_OVERHEAD &&
          payload.length <= MAX_AUDIO_PART_BYTES + ENCRYPTED_PAYLOAD_OVERHEAD,
      );
      parts[index] = await decryptPayload(
        root,
        slug,
        index,
        audio.parts,
        "chunk",
        payload,
      );
    }
  };
  await Promise.all(
    Array.from({ length: Math.min(AUDIO_FETCHES, audio.parts) }, worker),
  );
  requireValue(
    parts.reduce((total, part) => total + part.length, 0) === audio.bytes,
  );
  return new Blob(parts, { type: audio.type });
}

function pageLocation(pathname) {
  const segments = pathname.split("/");
  const last = segments.at(-1);
  if (last === "" || last === "index.html") segments.pop();
  const slug = segments.at(-1);
  if (slug === undefined || !slugPattern.test(slug)) return null;
  return { slug, directory: new URL(`${segments.join("/")}/`, location.origin) };
}

async function openPublished() {
  let root;
  try {
    root = parseFragment(location.hash);
  } catch {
    showState(STATES.missingKey);
    return;
  }
  try {
    const place = pageLocation(location.pathname);
    if (place === null) {
      showState(STATES.gone);
      return;
    }
    let payload;
    try {
      const response = await fetch(
        new URL("page.sona", place.directory),
        fetchOptions,
      );
      if (response.status === 404) {
        showState(STATES.gone);
        return;
      }
      if (!response.ok) {
        showState(STATES.offline);
        return;
      }
      payload = new Uint8Array(await response.arrayBuffer());
    } catch {
      showState(STATES.offline);
      return;
    }
    if (
      payload.length <= ENCRYPTED_PAYLOAD_OVERHEAD ||
      payload.length > MAX_PAGE_BYTES + ENCRYPTED_PAYLOAD_OVERHEAD
    ) {
      showState(STATES.unreadable);
      return;
    }
    let plaintext;
    try {
      plaintext = await decryptPayload(
        root,
        place.slug,
        0,
        1,
        "manifest",
        payload,
      );
    } catch {
      showState(STATES.wrongKey);
      return;
    }
    let page;
    try {
      page = parsePage(JSON.parse(decoder.decode(plaintext)));
    } catch {
      showState(STATES.unreadable);
      return;
    } finally {
      plaintext.fill(0);
    }
    if (page.audio !== null) recordingNote("Loading the recording");
    renderPage(page);
    if (page.audio === null) return;
    await nextPaint();
    try {
      const blob = await loadAudio(root, place.slug, place.directory, page.audio);
      const audio = document.createElement("audio");
      audio.src = URL.createObjectURL(blob);
      useAudio(audio);
    } catch {
      recordingFailed();
    }
  } finally {
    root.fill(0);
  }
}

function previewAudioName() {
  const element = document.getElementById("sona-audio");
  if (element === null) return null;
  try {
    const name = JSON.parse(element.textContent);
    return isJsonString(name) && localFileName.test(name) ? name : null;
  } catch {
    return null;
  }
}

function renderPreview(source, audioName) {
  let page;
  try {
    const json = JSON.stringify(source);
    requireValue(utf8(json).length <= MAX_PAGE_BYTES);
    page = parsePage(JSON.parse(json));
  } catch {
    showState(STATES.unreadable);
    return false;
  }
  if (page.audio === null) {
    if (recording.audio !== null) recording.audio.pause();
  } else if (audioName === null) {
    recordingNote("The recording is not in this preview.");
  } else if (recording.audio === null) {
    const audio = document.createElement("audio");
    audio.src = audioName;
    useAudio(audio);
  }
  renderPage(page);
  return true;
}

function openPreview(inline) {
  const audioName = previewAudioName();
  window.sonaRender = (page) => renderPreview(page, audioName);
  let page;
  try {
    page = JSON.parse(inline.textContent);
  } catch {
    showState(STATES.unreadable);
    return;
  }
  renderPreview(page, audioName);
}

const inlinePage = document.getElementById("sona-page");
if (inlinePage === null) void openPublished();
else openPreview(inlinePage);
