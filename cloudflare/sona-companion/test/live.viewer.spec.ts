import {
  afterAll,
  afterEach,
  beforeAll,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import { ws, type WebSocketHandlerConnection } from "msw";
import { setupServer } from "msw/node";

import pageHtml from "../public/index.html?raw";
import fixture from "../fixtures/crypto-v1.json";
import { LIVE_ROOM_GONE } from "../src/constants";

const vectors = fixture.live_room_aes_gcm_hkdf;
const ROOM = vectors.room_id;
const ROOT = vectors.root;
const JOIN_TOKEN = vectors.join_token;
const CONTENT_KEY = vectors.frame_key;

interface LivePoint {
  id: string;
  text: string;
  said_ns: number | null;
  edited: boolean;
}

interface LiveCard {
  id: string;
  what: string;
  owner: string | null;
  due_at_ms: number | null;
  due_words: string | null;
  said_ns: number;
  edited: boolean;
}

interface LiveLine {
  said_ns: number;
  who: "You" | "Call";
  text: string;
}

interface LiveChange {
  id: string;
  by: "sona" | "owner" | "teammate";
  name?: string | null;
  at_ms: number;
  call_ns: number | null;
  target: "point" | "card";
  target_id: string;
  kind: "added" | "rewrote" | "edited" | "removed";
  text: string;
}

interface LiveState {
  v: 1;
  title: string;
  live: boolean;
  heard_ns: number;
  points: LivePoint[];
  cards: LiveCard[];
  lines: LiveLine[];
  changes: LiveChange[];
}

type LiveFrame =
  | { type: "presence"; host: boolean }
  | { type: "ended" }
  | { type: "state"; seq: number; box: string; final: boolean };

type LiveClient = WebSocketHandlerConnection["client"];

function deferredAt<T>(list: PromiseWithResolvers<T>[], index: number) {
  while (list.length <= index) list.push(Promise.withResolvers<T>());
  return required(list[index]);
}

// The page's own socket, so a frame or close can be awaited until the page's
// listener, registered first, has handled it.
function pageSocket(client: LiveClient): EventTarget {
  if (!("socket" in client) || !(client.socket instanceof EventTarget))
    throw new Error("msw client carries no page socket");
  return client.socket;
}

class LiveRoom {
  readonly sent: string[] = [];
  closed = false;
  readonly #arrivals: PromiseWithResolvers<string>[] = [];
  readonly #page: EventTarget;

  constructor(readonly client: LiveClient) {
    this.#page = pageSocket(client);
    this.#page.addEventListener("close", () => {
      this.closed = true;
    });
    client.addEventListener("message", (event) => {
      const data = String(event.data);
      deferredAt(this.#arrivals, this.sent.length).resolve(data);
      this.sent.push(data);
    });
  }

  sentAt(index: number): Promise<string> {
    return deferredAt(this.#arrivals, index).promise;
  }

  async receive(frame: LiveFrame): Promise<void> {
    const handled = Promise.withResolvers<void>();
    this.#page.addEventListener("message", () => handled.resolve(), {
      once: true,
    });
    this.client.send(JSON.stringify(frame));
    await handled.promise;
  }

  async close(code: number): Promise<void> {
    const handled = Promise.withResolvers<void>();
    this.#page.addEventListener("close", () => handled.resolve(), {
      once: true,
    });
    this.client.close(code);
    await handled.promise;
  }
}

const rooms: PromiseWithResolvers<LiveRoom>[] = [];
const connected: LiveRoom[] = [];
let connections = 0;

const server = setupServer(
  ws
    .link(`ws://${location.host}/v1/live/*`)
    .addEventListener("connection", ({ client }) => {
      const room = new LiveRoom(client);
      connected.push(room);
      const index = connections;
      connections += 1;
      client.addEventListener(
        "open",
        () => deferredAt(rooms, index).resolve(room),
        { once: true },
      );
    }),
);

// Resolves once the page has opened its index-th socket since the test began.
function roomAt(index: number): Promise<LiveRoom> {
  return deferredAt(rooms, index).promise;
}

const SECOND = 1_000_000_000;
const lampsPoint: LivePoint = {
  id: "p-lamps",
  text: "Replace the pier lamps on berth three",
  said_ns: 5 * SECOND,
  edited: false,
};
const mapPoint: LivePoint = {
  id: "p-map",
  text: "Map review notes go to the shared folder",
  said_ns: 30 * SECOND,
  edited: true,
};
const quoteCard: LiveCard = {
  id: "c-quote",
  what: "Call the electrician and send the quote",
  owner: "Priya",
  due_at_ms: Date.UTC(2026, 10, 13, 17),
  due_words: "by Thursday",
  said_ns: 50 * SECOND,
  edited: false,
};
const followCard: LiveCard = {
  id: "c-follow",
  what: "Set up the Monday follow-up",
  owner: null,
  due_at_ms: null,
  due_words: null,
  said_ns: 60 * SECOND,
  edited: false,
};

function liveState(title: string): LiveState {
  return {
    v: 1,
    title,
    live: true,
    heard_ns: 754 * SECOND,
    points: [lampsPoint, mapPoint],
    cards: [quoteCard, followCard],
    lines: [],
    changes: [],
  };
}

function decodeBase64Url(value: string): Uint8Array {
  const padded =
    value.replaceAll("-", "+").replaceAll("_", "/") +
    "=".repeat((4 - (value.length % 4)) % 4);
  return Uint8Array.from(atob(padded), (char) => char.charCodeAt(0));
}

function encodeBase64Url(value: Uint8Array): string {
  return btoa(String.fromCharCode(...value))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

function liveAad(kind: "state" | "edit", seq: number): Uint8Array {
  const parts = ["sona-live-aad-v1", ROOM, kind, String(seq)].map((part) =>
    new TextEncoder().encode(part),
  );
  const out = new Uint8Array(
    parts.reduce((total, part) => total + 4 + part.length, 0),
  );
  const view = new DataView(out.buffer);
  let offset = 0;
  for (const part of parts) {
    view.setUint32(offset, part.length);
    out.set(part, offset + 4);
    offset += 4 + part.length;
  }
  return out;
}

async function contentKey(): Promise<CryptoKey> {
  return crypto.subtle.importKey(
    "raw",
    decodeBase64Url(CONTENT_KEY),
    "AES-GCM",
    false,
    ["encrypt", "decrypt"],
  );
}

async function seal(aad: Uint8Array, plaintext: string): Promise<string> {
  const nonce = crypto.getRandomValues(new Uint8Array(12));
  const sealed = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: nonce, additionalData: aad },
      await contentKey(),
      new TextEncoder().encode(plaintext),
    ),
  );
  const box = new Uint8Array(nonce.length + sealed.length);
  box.set(nonce);
  box.set(sealed, nonce.length);
  return encodeBase64Url(box);
}

async function unseal(aad: Uint8Array, box: string): Promise<string> {
  const bytes = decodeBase64Url(box);
  const plaintext = await crypto.subtle.decrypt(
    { name: "AES-GCM", iv: bytes.subarray(0, 12), additionalData: aad },
    await contentKey(),
    bytes.subarray(12),
  );
  return new TextDecoder().decode(plaintext);
}

async function stateFrame(
  seq: number,
  state: LiveState | { v: 1; removed: true },
  final = false,
  sealedFor = seq,
): Promise<LiveFrame> {
  return {
    type: "state",
    seq,
    box: await seal(liveAad("state", sealedFor), JSON.stringify(state)),
    final,
  };
}

function required<T>(value: T | null | undefined): T {
  if (value === null || value === undefined) throw new Error("missing");
  return value;
}

function byId(id: string): HTMLElement {
  return required(document.getElementById(id));
}

function rowField(list: string, index: number): HTMLTextAreaElement {
  return required(
    document.querySelectorAll<HTMLTextAreaElement>(`#${list} li textarea`)[
      index
    ],
  );
}

function rowMark(list: string, index: number): string | null {
  return required(document.querySelectorAll(`#${list} li .live-mark`)[index])
    .textContent;
}

function edit(field: HTMLTextAreaElement, text: string): void {
  field.focus();
  field.value = text;
  field.dispatchEvent(new Event("input", { bubbles: true }));
  field.dispatchEvent(
    new KeyboardEvent("keydown", {
      key: "Enter",
      bubbles: true,
      cancelable: true,
    }),
  );
}

async function sentEdit(
  room: LiveRoom,
  index: number,
  seq: number,
): Promise<string> {
  const match = new RegExp(
    `^\\{"type":"edit","seq":${seq},"box":"([A-Za-z0-9_-]+)"\\}$`,
    "u",
  ).exec(await room.sentAt(index));
  return unseal(liveAad("edit", seq), required(match?.[1]));
}

function showLivePage(path: string): void {
  const page = new DOMParser().parseFromString(pageHtml, "text/html");
  document.body.replaceChildren(
    ...Array.from(page.body.childNodes, (node) =>
      document.importNode(node, true),
    ),
  );
  history.replaceState(null, "", path);
}

beforeAll(() => server.listen({ onUnhandledFrame: "error" }));
afterAll(() => server.close());

afterEach(async () => {
  vi.useRealTimers();
  // Gone, so no page from this test reconnects into the next one.
  await Promise.all(
    connected
      .filter((room) => !room.closed)
      .map((room) => room.close(LIVE_ROOM_GONE)),
  );
  connected.length = 0;
  document.body.replaceChildren();
  history.replaceState(null, "", "/");
  sessionStorage.clear();
  localStorage.clear();
  rooms.length = 0;
  connections = 0;
});

// Each query loads a fresh copy of the page script, which boots on load.
describe("live doc page", () => {
  it("joins with the contract token, drops the key from the address, and keeps it for a reload", async () => {
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-join-test");
    const url = (await roomAt(0)).client.url;
    expect(url.protocol).toBe("ws:");
    expect(url.pathname).toBe(`/v1/live/${ROOM}/socket`);
    expect(url.search).toBe(`?t=${JOIN_TOKEN}`);
    expect(location.hash).toBe("");
    expect(location.pathname).toBe(`/l/${ROOM}`);

    showLivePage(`/l/${ROOM}`);
    await import("../public/viewer.js?live-reload-test");
    expect((await roomAt(1)).client.url.search).toBe(`?t=${JOIN_TOKEN}`);
  });

  it("renders a state sealed for its seq and ignores forged or stale ones", async () => {
    expect(encodeBase64Url(liveAad("state", vectors.state.seq))).toBe(
      vectors.state.aad,
    );
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-order-test");
    const socket = await roomAt(0);
    const title = byId("live-title");
    const titles: (string | null)[] = [];
    new MutationObserver(() => titles.push(title.textContent)).observe(title, {
      childList: true,
      characterData: true,
      subtree: true,
    });
    await socket.receive({ type: "presence", host: true });

    await socket.receive(
      await stateFrame(5, liveState("Forged agenda"), false, 4),
    );
    await socket.receive(
      await stateFrame(3, liveState("Harbor walk follow-up")),
    );
    await vi.waitFor(() =>
      expect(title.textContent).toBe("Harbor walk follow-up"),
    );
    expect(document.title).toBe("Harbor walk follow-up — Sona");
    expect(rowField("live-points", 0).value).toBe(lampsPoint.text);

    await socket.receive(await stateFrame(2, liveState("Old agenda")));
    await socket.receive(await stateFrame(6, liveState("Final agenda")));
    await vi.waitFor(() => expect(title.textContent).toBe("Final agenda"));
    expect(titles).not.toContain("Forged agenda");
    expect(titles).not.toContain("Old agenda");
  });

  it("opens the state box the Mac sealed for the shared vectors", async () => {
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-vector-test");
    const socket = await roomAt(0);
    await socket.receive({ type: "presence", host: true });
    await socket.receive({
      type: "state",
      seq: vectors.state.seq,
      box: vectors.state.box,
      final: false,
    });
    await vi.waitFor(() =>
      expect(byId("live-title").textContent).toBe("Fixture"),
    );
    expect(rowField("live-points", 0).value).toBe("Launch moves to March 14");
    expect(byId("live-clock").textContent).toBe(" · 1:05 into the call");
  });

  it("shows who changed each item and merges changes into the timeline by call time", async () => {
    localStorage.setItem("sona-live-name", "Priya");
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-render-test");
    const socket = await roomAt(0);
    await socket.receive({ type: "presence", host: true });
    await socket.receive(
      await stateFrame(1, {
        ...liveState("Harbor walk follow-up"),
        heard_ns: 3920 * SECOND,
        lines: [
          { said_ns: 10 * SECOND, who: "You", text: "Start with the lamps." },
          {
            said_ns: 50 * SECOND,
            who: "Call",
            text: "I can send the quote by Thursday.",
          },
        ],
        changes: [
          {
            id: "change-map",
            by: "teammate",
            name: "Priya",
            at_ms: 1_000,
            call_ns: 42 * SECOND,
            target: "point",
            target_id: mapPoint.id,
            kind: "edited",
            text: mapPoint.text,
          },
          {
            id: "change-follow",
            by: "teammate",
            at_ms: 3_000,
            call_ns: null,
            target: "card",
            target_id: followCard.id,
            kind: "edited",
            text: followCard.what,
          },
          {
            id: "change-lamps",
            by: "sona",
            name: null,
            at_ms: 500,
            call_ns: 5 * SECOND,
            target: "point",
            target_id: lampsPoint.id,
            kind: "added",
            text: lampsPoint.text,
          },
          {
            id: "change-quote",
            by: "owner",
            name: null,
            at_ms: 2_000,
            call_ns: 3909 * SECOND,
            target: "card",
            target_id: quoteCard.id,
            kind: "rewrote",
            text: quoteCard.what,
          },
        ],
      }),
    );
    await vi.waitFor(() =>
      expect(byId("live-status").textContent).toBe("Live"),
    );
    expect(byId("live-clock").textContent).toBe(" · 1:05:20 into the call");
    expect(rowMark("live-points", 0)).toBe("Sona · 0:05");
    expect(rowMark("live-points", 1)).toBe("Priya · 0:42");
    expect(rowMark("live-cards", 0)).toBe("Host · 1:05:09");
    expect(rowMark("live-cards", 1)).toBe("A teammate · After the call");
    const day = new Intl.DateTimeFormat(undefined, {
      weekday: "short",
      month: "short",
      day: "numeric",
    }).format(required(quoteCard.due_at_ms));
    expect(
      Array.from(
        document.querySelectorAll("#live-cards .live-card-meta"),
        (meta) => meta.textContent,
      ),
    ).toEqual([`Priya · ${day}`, "No owner"]);
    expect(
      Array.from(
        document.querySelectorAll("#live-timeline-list .live-entry-meta"),
        (meta) => meta.textContent,
      ),
    ).toEqual([
      "0:05 · Sona added a point",
      "0:10 · Host",
      "0:42 · Priya changed a point",
      "0:50 · Call",
      "1:05:09 · Host rewrote a card",
      "After the call · A teammate changed a card",
    ]);
  });

  it("seals each edit for its own seq and confirms it when the change lands", async () => {
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-edit-test");
    const socket = await roomAt(0);
    await socket.receive({ type: "presence", host: true });
    await socket.receive(
      await stateFrame(3, liveState("Harbor walk follow-up")),
    );
    await vi.waitFor(() => expect(byId("live-name-form").hidden).toBe(false));
    expect(rowField("live-points", 0).readOnly).toBe(true);

    const nameInput = required(
      document.querySelector<HTMLInputElement>("#live-name-input"),
    );
    nameInput.value = "  Priya   Raman ";
    required(
      document.querySelector<HTMLFormElement>("#live-name-form"),
    ).requestSubmit();
    expect(byId("live-editor-name").textContent).toBe("Editing as Priya Raman");
    expect(localStorage.getItem("sona-live-name")).toBe("Priya Raman");

    const lamps = rowField("live-points", 0);
    expect(lamps.readOnly).toBe(false);
    edit(lamps, "Replace the pier  lamps on berth four ");
    const first =
      /^\{"v":1,"id":"([A-Za-z0-9_-]{22})","name":"Priya Raman","target":"point","target_id":"p-lamps","text":"Replace the pier lamps on berth four"\}$/u.exec(
        await sentEdit(socket, 0, 1),
      );
    const editId = required(first?.[1]);
    expect(rowMark("live-points", 0)).toBe("Sending");

    await socket.receive(
      await stateFrame(4, {
        ...liveState("Harbor walk follow-up"),
        points: [
          { ...lampsPoint, text: "Replace the pier lamps on berth four" },
          mapPoint,
        ],
        changes: [
          {
            id: editId,
            by: "teammate",
            name: "Priya Raman",
            at_ms: 5_000,
            call_ns: 61 * SECOND,
            target: "point",
            target_id: lampsPoint.id,
            kind: "edited",
            text: "Replace the pier lamps on berth four",
          },
        ],
      }),
    );
    await vi.waitFor(() =>
      expect(rowMark("live-points", 0)).toBe("Priya Raman · 1:01"),
    );

    required(
      document.querySelectorAll<HTMLButtonElement>(
        "#live-points .live-remove",
      )[1],
    ).click();
    expect(await sentEdit(socket, 1, 2)).toMatch(
      /^\{"v":1,"id":"[A-Za-z0-9_-]{22}","name":"Priya Raman","target":"point","target_id":"p-map","text":""\}$/u,
    );
    expect(rowMark("live-points", 1)).toBe("Removing");
  });

  it("marks an edit not saved when the call ends without it", async () => {
    localStorage.setItem("sona-live-name", "Priya");
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-ended-test");
    const socket = await roomAt(0);
    await socket.receive({ type: "presence", host: true });
    await socket.receive(
      await stateFrame(1, liveState("Harbor walk follow-up")),
    );
    await vi.waitFor(() =>
      expect(rowField("live-cards", 0).readOnly).toBe(false),
    );

    edit(rowField("live-cards", 0), "Call the electrician on Friday");
    await sentEdit(socket, 0, 1);
    await socket.receive(
      await stateFrame(
        2,
        { ...liveState("Harbor walk follow-up"), live: false },
        true,
      ),
    );
    await vi.waitFor(() => expect(rowMark("live-cards", 0)).toBe("Not saved"));
    expect(byId("live-status").textContent).toBe(
      "The call ended. This doc is read-only.",
    );
    expect(rowField("live-cards", 0).value).toBe(quoteCard.what);
    expect(rowField("live-cards", 0).readOnly).toBe(true);
    expect(byId("live-add").hidden).toBe(true);
    expect(byId("live-editor").hidden).toBe(true);
  });

  it("confirms an edit that lands after the page gave up on it", async () => {
    localStorage.setItem("sona-live-name", "Priya");
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-late-test");
    const socket = await roomAt(0);
    await socket.receive({ type: "presence", host: true });
    await socket.receive(
      await stateFrame(1, liveState("Harbor walk follow-up")),
    );
    await vi.waitFor(() =>
      expect(rowField("live-points", 0).readOnly).toBe(false),
    );

    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    edit(rowField("live-points", 0), "Replace the pier lamps on berth four");
    const editId = required(
      /"id":"([A-Za-z0-9_-]{22})"/u.exec(await sentEdit(socket, 0, 1))?.[1],
    );
    vi.advanceTimersByTime(20_000);
    expect(rowMark("live-points", 0)).toBe("Not saved");

    await socket.receive(
      await stateFrame(2, {
        ...liveState("Harbor walk follow-up"),
        points: [
          { ...lampsPoint, text: "Replace the pier lamps on berth four" },
          mapPoint,
        ],
        changes: [
          {
            id: editId,
            by: "teammate",
            name: "Priya",
            at_ms: 5_000,
            call_ns: 61 * SECOND,
            target: "point",
            target_id: lampsPoint.id,
            kind: "edited",
            text: "Replace the pier lamps on berth four",
          },
        ],
      }),
    );
    await vi.waitFor(() =>
      expect(rowMark("live-points", 0)).toBe("Priya · 1:01"),
    );
  });

  it("marks an edit not saved when the host leaves before it is sent", async () => {
    localStorage.setItem("sona-live-name", "Priya");
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-unsent-test");
    const socket = await roomAt(0);
    await socket.receive({ type: "presence", host: true });
    await socket.receive(
      await stateFrame(1, liveState("Harbor walk follow-up")),
    );
    await vi.waitFor(() =>
      expect(rowField("live-points", 0).readOnly).toBe(false),
    );

    const lamps = rowField("live-points", 0);
    lamps.value = "Replace the pier lamps on berth four";
    lamps.dispatchEvent(new Event("input", { bubbles: true }));
    await socket.receive({ type: "presence", host: false });
    lamps.dispatchEvent(new FocusEvent("blur"));
    expect(rowMark("live-points", 0)).toBe("Not saved");
    expect(rowField("live-points", 0).value).toBe(
      "Replace the pier lamps on berth four",
    );
    expect(socket.sent).toHaveLength(0);

    await socket.receive({ type: "presence", host: true });
    edit(rowField("live-points", 0), "Replace the pier lamps on berth four");
    expect(await sentEdit(socket, 0, 1)).toMatch(
      /"target_id":"p-lamps","text":"Replace the pier lamps on berth four"\}$/u,
    );
  });

  it("keeps the names that mark the call's own changes", async () => {
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-reserved-test");
    const socket = await roomAt(0);
    await socket.receive({ type: "presence", host: true });
    await socket.receive(
      await stateFrame(1, liveState("Harbor walk follow-up")),
    );
    await vi.waitFor(() => expect(byId("live-name-form").hidden).toBe(false));

    required(
      document.querySelector<HTMLInputElement>("#live-name-input"),
    ).value = " host ";
    required(
      document.querySelector<HTMLFormElement>("#live-name-form"),
    ).requestSubmit();
    expect(byId("live-name-hint").textContent).toBe(
      "Enter your own name to edit",
    );
    expect(byId("live-name-form").hidden).toBe(false);
    expect(localStorage.getItem("sona-live-name")).toBeNull();
  });

  it("empties the page once the host deletes the meeting, even from a later launch", async () => {
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-removed-test");
    const socket = await roomAt(0);
    await socket.receive({ type: "presence", host: true });
    await socket.receive(
      await stateFrame(
        1,
        { ...liveState("Harbor walk follow-up"), live: false },
        true,
      ),
    );
    await vi.waitFor(() =>
      expect(byId("live-status").textContent).toBe(
        "The call ended. This doc is read-only.",
      ),
    );

    await socket.receive(
      await stateFrame(Number.MAX_SAFE_INTEGER, { v: 1, removed: true }, true),
    );
    await vi.waitFor(() =>
      expect(byId("live-status").textContent).toBe(
        "The host removed this doc.",
      ),
    );
    expect(byId("live-layout").hidden).toBe(true);
    expect(byId("live-timeline").hidden).toBe(true);
    expect(document.title).toBe("Live doc — Sona");
  });

  it("retries a dropped socket but stops and says the link expired once the room is gone", async () => {
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-gone-test");
    const first = await roomAt(0);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    await first.close(1001);
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(1_000);

    await (await roomAt(1)).close(LIVE_ROOM_GONE);
    expect(byId("live-status").textContent).toBe("This link has expired.");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("pauses editing while the host is away", async () => {
    localStorage.setItem("sona-live-name", "Priya");
    showLivePage(`/l/${ROOM}#v=1&k=${ROOT}`);
    await import("../public/viewer.js?live-host-test");
    const socket = await roomAt(0);
    await socket.receive({ type: "presence", host: false });
    await socket.receive(
      await stateFrame(1, liveState("Harbor walk follow-up")),
    );
    await vi.waitFor(() =>
      expect(byId("live-status").textContent).toBe(
        "The host is offline. Edits are paused.",
      ),
    );
    expect(rowField("live-points", 0).readOnly).toBe(true);
    expect(byId("live-add").hidden).toBe(true);

    await socket.receive({ type: "presence", host: true });
    expect(byId("live-status").textContent).toBe("Live");
    expect(rowField("live-points", 0).readOnly).toBe(false);
    expect(byId("live-add").hidden).toBe(false);
  });
});
