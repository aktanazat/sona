import {
  env,
  evictDurableObject,
  runDurableObjectAlarm,
  SELF,
} from "cloudflare:test";
import { afterEach, describe, expect, it } from "vitest";

import fixture from "../fixtures/crypto-v1.json";
import {
  base64UrlDecode,
  base64UrlEncode,
  sha256Base64Url,
} from "../src/encoding";
import {
  bootstrap,
  jsonBody,
  makeDevice,
  signedFetch,
  testId,
  type Device,
} from "./helpers";

interface Room {
  host: Uint8Array;
  join: Uint8Array;
  roomId: string;
}

interface Peer {
  closed: Promise<number>;
  next(): Promise<string>;
  queued: string[];
  socket: WebSocket;
}

const openSockets: WebSocket[] = [];

afterEach(() => {
  for (const socket of openSockets.splice(0)) {
    if (socket.readyState === WebSocket.OPEN) socket.close(1000, "Test over");
  }
});

function randomBytes(length: number): Uint8Array {
  const value = new Uint8Array(length);
  crypto.getRandomValues(value);
  return value;
}

function newRoom(): Room {
  return {
    host: randomBytes(32),
    join: randomBytes(32),
    roomId: testId("room"),
  };
}

async function createRoom(
  device: Device,
  room: Room,
  idempotencyKey = testId("live"),
): Promise<Response> {
  return signedFetch(device, {
    method: "POST",
    path: "/v1/live",
    idempotencyKey,
    body: JSON.stringify({
      version: 1,
      roomId: room.roomId,
      hostVerifier: await sha256Base64Url(room.host),
      joinVerifier: await sha256Base64Url(room.join),
    }),
  });
}

async function liveRoom(): Promise<Room> {
  const device = await makeDevice();
  await bootstrap(device);
  const room = newRoom();
  expect((await createRoom(device, room)).status).toBe(200);
  return room;
}

function socketRequest(
  roomId: string,
  token: Uint8Array | string,
): Promise<Response> {
  const t = token instanceof Uint8Array ? base64UrlEncode(token) : token;
  return SELF.fetch(`https://companion.test/v1/live/${roomId}/socket?t=${t}`, {
    headers: { Upgrade: "websocket" },
  });
}

async function connect(roomId: string, token: Uint8Array): Promise<Peer> {
  const response = await socketRequest(roomId, token);
  const socket = response.webSocket;
  if (socket === null)
    throw new Error(`expected a socket, got ${response.status}`);
  socket.accept();
  openSockets.push(socket);
  const queued: string[] = [];
  const waiting: ((text: string) => void)[] = [];
  socket.addEventListener("message", (event) => {
    const text = String(event.data);
    const deliver = waiting.shift();
    if (deliver === undefined) queued.push(text);
    else deliver(text);
  });
  return {
    closed: new Promise((resolve) =>
      socket.addEventListener("close", (event) => resolve(event.code)),
    ),
    next: () => {
      const text = queued.shift();
      if (text !== undefined) return Promise.resolve(text);
      return new Promise((resolve) => waiting.push(resolve));
    },
    queued,
    socket,
  };
}

function presence(host: boolean): string {
  return JSON.stringify({ type: "presence", host });
}

function stateFrame(seq: number, final: boolean): string {
  const box = base64UrlEncode(randomBytes(40));
  return JSON.stringify({ type: "state", seq, box, final });
}

function editFrame(seq: number): string {
  return JSON.stringify({
    type: "edit",
    seq,
    box: base64UrlEncode(randomBytes(40)),
  });
}

function frameOfSize(bytes: number, fields: (box: string) => string): string {
  const frame = fields("A".repeat(bytes - fields("").length));
  expect(frame.length).toBe(bytes);
  return frame;
}

describe("live rooms", () => {
  it("creates a room only for a signed, known device", async () => {
    const room = newRoom();
    const body = JSON.stringify({
      version: 1,
      roomId: room.roomId,
      hostVerifier: await sha256Base64Url(room.host),
      joinVerifier: await sha256Base64Url(room.join),
    });
    const unsigned = await SELF.fetch("https://companion.test/v1/live", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "content-length": String(body.length),
        "x-sona-idempotency-key": testId("live"),
      },
      body,
    });
    expect(unsigned.status).toBe(401);
    expect((await createRoom(await makeDevice(), room)).status).toBe(401);
    expect((await socketRequest(room.roomId, room.host)).status).toBe(404);

    const device = await makeDevice();
    await bootstrap(device);
    const before = Date.now();
    const created = await createRoom(device, room);
    expect(created.status).toBe(200);
    const response = await jsonBody(created);
    expect(response.room_id).toBe(room.roomId);
    expect(response.page_url).toBe(`https://meet.aktan.us/l/${room.roomId}`);
    expect(response.expires_at).toBeGreaterThanOrEqual(
      before + 7 * 24 * 60 * 60 * 1000,
    );
    expect(response.expires_at).toBeLessThanOrEqual(
      Date.now() + 7 * 24 * 60 * 60 * 1000,
    );
  });

  it("answers a replay with the stored response and never revives an expired room", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const room = newRoom();
    const key = testId("live");
    const created = await createRoom(device, room, key);
    expect(created.status).toBe(200);
    const stored = await created.text();

    const taken = await createRoom(device, {
      ...newRoom(),
      roomId: room.roomId,
    });
    expect(taken.status).toBe(409);
    expect((await jsonBody(taken)).code).toBe("idempotency_conflict");

    expect(
      await runDurableObjectAlarm(env.LIVE_ROOMS.getByName(room.roomId)),
    ).toBe(true);
    expect((await socketRequest(room.roomId, room.join)).status).toBe(404);

    const replay = await createRoom(device, room, key);
    expect(replay.status).toBe(200);
    expect(await replay.text()).toBe(stored);
    expect((await socketRequest(room.roomId, room.join)).status).toBe(404);
  });

  it("opens the host and guest roles for the contract's test vector tokens", async () => {
    const device = await makeDevice();
    await bootstrap(device);
    const vectors = fixture.live_room_aes_gcm_hkdf;
    const created = await signedFetch(device, {
      method: "POST",
      path: "/v1/live",
      idempotencyKey: testId("live"),
      body: JSON.stringify({
        version: 1,
        roomId: vectors.room_id,
        hostVerifier: vectors.host_verifier,
        joinVerifier: vectors.join_verifier,
      }),
    });
    expect(created.status).toBe(200);
    const joinToken = base64UrlDecode(vectors.join_token);
    const hostToken = base64UrlDecode(vectors.host_token);
    if (joinToken === null || hostToken === null)
      throw new Error("bad vector token");

    const guest = await connect(vectors.room_id, joinToken);
    expect(await guest.next()).toBe(presence(false));
    await connect(vectors.room_id, hostToken);
    expect(await guest.next()).toBe(presence(true));
  });

  it("refuses a token that matches neither verifier and an unknown room", async () => {
    const room = await liveRoom();
    for (const token of [
      randomBytes(32),
      base64UrlEncode(room.join).slice(1),
      "",
    ]) {
      const response = await socketRequest(room.roomId, token);
      expect(response.status).toBe(403);
      expect((await jsonBody(response)).code).toBe("unauthorized");
    }
    const unknown = await socketRequest(testId("room"), room.join);
    expect(unknown.status).toBe(404);
  });

  it("gives a guest who connects later the presence, then the newest stored state", async () => {
    const room = await liveRoom();
    const host = await connect(room.roomId, room.host);
    const watcher = await connect(room.roomId, room.join);
    expect(await watcher.next()).toBe(presence(true));
    const second = stateFrame(2, false);
    const newest = stateFrame(3, false);
    host.socket.send(second);
    host.socket.send(stateFrame(1, false));
    host.socket.send(newest);
    expect(await watcher.next()).toBe(second);
    expect(await watcher.next()).toBe(newest);

    await evictDurableObject(env.LIVE_ROOMS.getByName(room.roomId));
    const later = await connect(room.roomId, room.join);
    expect(await later.next()).toBe(presence(true));
    expect(await later.next()).toBe(newest);
  });

  it("relays a guest edit to the host unchanged", async () => {
    const room = await liveRoom();
    const host = await connect(room.roomId, room.host);
    const guest = await connect(room.roomId, room.join);
    expect(await guest.next()).toBe(presence(true));
    const edit = editFrame(1);
    guest.socket.send(edit);
    expect(await host.next()).toBe(edit);
  });

  it("tells a guest when the host is away and drops its edit", async () => {
    const room = await liveRoom();
    const guest = await connect(room.roomId, room.join);
    expect(await guest.next()).toBe(presence(false));
    guest.socket.send(editFrame(1));
    expect(await guest.next()).toBe(presence(false));

    const host = await connect(room.roomId, room.host);
    expect(await guest.next()).toBe(presence(true));
    host.socket.close(1000, "Call over");
    expect(await guest.next()).toBe(presence(false));
    expect(host.queued).toEqual([]);
  });

  it("refuses guest edits after a final state and keeps later states final", async () => {
    const room = await liveRoom();
    const host = await connect(room.roomId, room.host);
    const guest = await connect(room.roomId, room.join);
    expect(await guest.next()).toBe(presence(true));
    const closing = stateFrame(1, true);
    host.socket.send(closing);
    expect(await guest.next()).toBe(closing);

    guest.socket.send(editFrame(1));
    expect(await guest.next()).toBe(JSON.stringify({ type: "ended" }));

    const box = base64UrlEncode(randomBytes(40));
    host.socket.send(
      JSON.stringify({ type: "state", seq: 2, box, final: false }),
    );
    const kept = { type: "state", seq: 2, box, final: true };
    expect(JSON.parse(await guest.next())).toEqual(kept);
    const later = await connect(room.roomId, room.join);
    expect(await later.next()).toBe(presence(true));
    expect(JSON.parse(await later.next())).toEqual(kept);
    expect(host.queued).toEqual([]);
  });

  it("closes a socket whose frame is over its cap", async () => {
    const room = await liveRoom();
    const host = await connect(room.roomId, room.host);
    const guest = await connect(room.roomId, room.join);
    expect(await guest.next()).toBe(presence(true));
    const largestEdit = frameOfSize(16 * 1024, (box) =>
      JSON.stringify({ type: "edit", seq: 1, box }),
    );
    guest.socket.send(largestEdit);
    expect(await host.next()).toBe(largestEdit);
    guest.socket.send(`${largestEdit} `);
    expect(await guest.closed).toBe(1009);

    const watcher = await connect(room.roomId, room.join);
    expect(await watcher.next()).toBe(presence(true));
    const largestState = frameOfSize(1000 * 1000, (box) =>
      JSON.stringify({ type: "state", seq: 10, box, final: false }),
    );
    host.socket.send(largestState);
    expect(await watcher.next()).toBe(largestState);
    host.socket.send(`${largestState} `);
    expect(await host.closed).toBe(1009);
  });

  it("lets a new host connection replace the old one", async () => {
    const room = await liveRoom();
    const first = await connect(room.roomId, room.host);
    const second = await connect(room.roomId, room.host);
    expect(await first.closed).toBe(4000);
    const guest = await connect(room.roomId, room.join);
    expect(await guest.next()).toBe(presence(true));
    const edit = editFrame(1);
    guest.socket.send(edit);
    expect(await second.next()).toBe(edit);
  });

  it("refuses a guest past the fiftieth", async () => {
    const room = await liveRoom();
    for (let index = 0; index < 50; index += 1)
      await connect(room.roomId, room.join);
    const refused = await socketRequest(room.roomId, room.join);
    expect(refused.status).toBe(429);
    expect((await jsonBody(refused)).code).toBe("rate_limited");
  });

  it("serves the live page with the socket origin in its policy", async () => {
    const page = await SELF.fetch(`https://companion.test/l/${testId("room")}`);
    expect(page.status).toBe(200);
    expect(page.headers.get("content-security-policy")).toContain(
      "connect-src 'self' wss://meet.aktan.us;",
    );
    await expect(page.text()).resolves.toContain('id="live-view"');
  });
});
