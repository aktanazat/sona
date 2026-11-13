import { DurableObject } from "cloudflare:workers";
import {
  MAX_LIVE_GUEST_FRAME_BYTES,
  MAX_LIVE_GUESTS,
  MAX_LIVE_HOST_FRAME_BYTES,
} from "./constants";
import { byteLength, equalSecret } from "./encoding";
import type { Env } from "./types";
import {
  isJsonInteger,
  isJsonRecord,
  isJsonString,
  type JsonValue,
} from "./validation";

export const LIVE_VERIFIER_HEADER = "x-sona-live-verifier";

export interface LiveRoomRecord {
  expiresAt: number;
  hostVerifier: string;
  joinVerifier: string;
  vaultId: string;
}

interface StoredState {
  final: boolean;
  frame: string;
  seq: number;
}

interface StateFrame {
  box: string;
  final: boolean;
  seq: number;
  type: "state";
}

interface EditFrame {
  box: string;
  seq: number;
  type: "edit";
}

type Role = "guest" | "host";

const hostAbsent = JSON.stringify({ type: "presence", host: false });
const hostPresent = JSON.stringify({ type: "presence", host: true });
const roomEnded = JSON.stringify({ type: "ended" });

function isBox(value: string): boolean {
  return /^[A-Za-z0-9_-]{38,}$/u.test(value) && value.length % 4 !== 1;
}

function parseFrame(text: string): StateFrame | EditFrame | null {
  let value: JsonValue;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (!isJsonRecord(value)) return null;
  const { box, final, seq, type } = value;
  if (!isJsonInteger(seq) || seq < 1 || !isJsonString(box) || !isBox(box))
    return null;
  const keyCount = Object.keys(value).length;
  if (type === "state" && keyCount === 4 && (final === true || final === false))
    return { box, final, seq, type };
  if (type === "edit" && keyCount === 3) return { box, seq, type };
  return null;
}

async function roleFor(
  room: LiveRoomRecord,
  verifier: string,
): Promise<Role | null> {
  if (await equalSecret(verifier, room.hostVerifier)) return "host";
  if (await equalSecret(verifier, room.joinVerifier)) return "guest";
  return null;
}

function send(socket: WebSocket, text: string): void {
  if (socket.readyState === WebSocket.OPEN) socket.send(text);
}

export class LiveRoom extends DurableObject<Env> {
  async create(room: LiveRoomRecord): Promise<number | null> {
    const existing = await this.room();
    if (existing === undefined) {
      await this.ctx.storage.put("room", room);
      await this.ctx.storage.setAlarm(room.expiresAt);
      return room.expiresAt;
    }
    const same =
      existing.vaultId === room.vaultId &&
      existing.hostVerifier === room.hostVerifier &&
      existing.joinVerifier === room.joinVerifier;
    return same ? existing.expiresAt : null;
  }

  async fetch(request: Request): Promise<Response> {
    const room = await this.room();
    if (room === undefined) return new Response(null, { status: 404 });
    const role = await roleFor(
      room,
      request.headers.get(LIVE_VERIFIER_HEADER) ?? "",
    );
    if (role === null) return new Response(null, { status: 403 });
    if (
      role === "guest" &&
      this.ctx.getWebSockets("guest").length >= MAX_LIVE_GUESTS
    ) {
      return new Response(null, { status: 429 });
    }
    const pair = new WebSocketPair();
    const server = pair[1];
    if (role === "host") {
      for (const previous of this.ctx.getWebSockets("host"))
        previous.close(4000, "Another host connected");
      this.ctx.acceptWebSocket(server, [role]);
      this.broadcast(hostPresent);
    } else {
      this.ctx.acceptWebSocket(server, [role]);
      server.send(this.openHost() === undefined ? hostAbsent : hostPresent);
      const state = await this.state();
      if (state !== undefined) server.send(state.frame);
    }
    return new Response(null, { status: 101, webSocket: pair[0] });
  }

  async webSocketMessage(
    socket: WebSocket,
    message: string | ArrayBuffer,
  ): Promise<void> {
    const host = this.ctx.getTags(socket).includes("host");
    const limit = host ? MAX_LIVE_HOST_FRAME_BYTES : MAX_LIVE_GUEST_FRAME_BYTES;
    const size =
      message instanceof ArrayBuffer ? message.byteLength : byteLength(message);
    if (size > limit) {
      socket.close(1009, "Frame too large");
      return;
    }
    if (message instanceof ArrayBuffer) return;
    const frame = parseFrame(message);
    if (frame === null) return;
    if (frame.type === "state" && host) await this.publish(frame, message);
    if (frame.type === "edit" && !host) await this.relay(socket, message);
  }

  webSocketClose(socket: WebSocket): void {
    if (
      this.ctx.getTags(socket).includes("host") &&
      this.openHost() === undefined
    )
      this.broadcast(hostAbsent);
  }

  async alarm(): Promise<void> {
    await this.expire();
  }

  private async publish(frame: StateFrame, text: string): Promise<void> {
    const stored = await this.state();
    if (stored !== undefined && frame.seq <= stored.seq) return;
    const final = frame.final || stored?.final === true;
    const relayed =
      final === frame.final
        ? text
        : JSON.stringify({
            type: "state",
            seq: frame.seq,
            box: frame.box,
            final,
          });
    await this.ctx.storage.put<StoredState>("state", {
      final,
      frame: relayed,
      seq: frame.seq,
    });
    this.broadcast(relayed);
  }

  private async relay(sender: WebSocket, text: string): Promise<void> {
    const state = await this.state();
    if (state?.final === true) {
      send(sender, roomEnded);
      return;
    }
    const host = this.openHost();
    if (host === undefined) {
      send(sender, hostAbsent);
      return;
    }
    send(host, text);
  }

  private broadcast(text: string): void {
    for (const guest of this.ctx.getWebSockets("guest")) send(guest, text);
  }

  private openHost(): WebSocket | undefined {
    return this.ctx
      .getWebSockets("host")
      .find((socket) => socket.readyState === WebSocket.OPEN);
  }

  private async room(): Promise<LiveRoomRecord | undefined> {
    const room = await this.ctx.storage.get<LiveRoomRecord>("room");
    if (room === undefined || room.expiresAt > Date.now()) return room;
    await this.expire();
    return undefined;
  }

  private state(): Promise<StoredState | undefined> {
    return this.ctx.storage.get<StoredState>("state");
  }

  private async expire(): Promise<void> {
    for (const socket of this.ctx.getWebSockets())
      socket.close(1001, "The room expired");
    await this.ctx.storage.deleteAll();
  }
}
