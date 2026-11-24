import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { z } from "zod";

const replyPacket = z
  .object({
    decision: z.literal("block"),
    reason: z.string().refine((value) => value.trim().length > 0),
  })
  .strict();
const permissionPacket = z
  .object({
    decision: z.enum(["allow", "deny"]),
  })
  .strict();
const questionPacket = z.union([
  z.object({ decision: z.literal("deny") }).strict(),
  z.object({ answers: z.array(z.array(z.string())) }).strict(),
]);
const failedResponse = z.object({ status: z.number().min(400) });

const MAX_FRAME_BYTES = 64 * 1024;

function executable() {
  return (
    process.env.SONA_AGENT_HOOK?.trim() ||
    fileURLToPath(new URL("../../../../MacOS/sona-agent-hook", import.meta.url))
  );
}

function launch(payload, interactive) {
  const bytes = Buffer.from(JSON.stringify(payload));
  if (bytes.length > MAX_FRAME_BYTES) return undefined;
  const child = spawn(
    executable(),
    ["opencode", ...(interactive ? ["--interactive"] : [])],
    {
      cwd: payload.workspace_root,
      stdio: ["pipe", "pipe", "ignore"],
      windowsHide: true,
    },
  );
  const closed = new Promise((resolve) => {
    child.once("error", () => resolve(1));
    child.once("close", (code) => resolve(code ?? 1));
  });
  child.stdin.on("error", () => {});
  if (interactive) {
    child.stdin.write(bytes);
    child.stdin.write("\n");
  } else {
    child.stdin.end(bytes);
  }
  return { child, closed };
}

async function firstPacket(stream) {
  const chunks = [];
  let size = 0;
  for await (const chunk of stream) {
    size += chunk.length;
    if (size > MAX_FRAME_BYTES + 1) return undefined;
    const newline = chunk.indexOf(10);
    if (newline >= 0) {
      chunks.push(chunk.subarray(0, newline));
      try {
        return JSON.parse(Buffer.concat(chunks).toString("utf8"));
      } catch {
        return undefined;
      }
    }
    chunks.push(chunk);
  }
  return undefined;
}

export function validPacket(event, packet) {
  if (event === "input_request") return replyPacket.safeParse(packet).success;
  if (event === "permission_request")
    return permissionPacket.safeParse(packet).success;
  if (event === "question_request")
    return questionPacket.safeParse(packet).success;
  return false;
}

export function assertAccepted(result) {
  if (result?.error || failedResponse.safeParse(result?.response).success) {
    throw new Error("OpenCode rejected the response");
  }
}

export function createBridge() {
  const instanceId = randomUUID();
  const instanceStartedAt = Date.now();
  const active = new Map();
  const children = new Set();
  let sequence = 0;
  let disposed = false;

  const stamp = () => ({
    schema_version: 1,
    instance_id: instanceId,
    instance_started_at_ms: instanceStartedAt,
    sequence: ++sequence,
  });

  const track = (exchange) => {
    if (!exchange) return undefined;
    children.add(exchange);
    void exchange.closed.then(() => children.delete(exchange));
    return exchange;
  };

  const cancel = (ticket, force = false) => {
    if (ticket.applying && !force) return;
    ticket.controller.abort();
    ticket.exchange?.child.stdin.end();
  };

  return {
    async publish(sessionId, event, fields, locate) {
      const order = stamp();
      const location = await locate(sessionId);
      if (disposed || !location) return;
      const exchange = track(
        launch(
          {
            ...fields,
            message: fields.message ?? location.title,
            ...order,
            event,
            session_id: sessionId,
            workspace_root: location.directory,
          },
          false,
        ),
      );
      if (exchange) await exchange.closed;
    },

    async request(sessionId, requestId, event, fields, locate, deliver) {
      const key = JSON.stringify([sessionId, event, requestId]);
      const previous = active.get(key);
      if (previous) cancel(previous, true);
      const ticket = {
        sessionId,
        requestId,
        event,
        controller: new AbortController(),
        applying: false,
        exchange: undefined,
      };
      const order = stamp();
      active.set(key, ticket);
      let accepted = false;
      let packet;
      const current = () =>
        !disposed &&
        !ticket.controller.signal.aborted &&
        active.get(key) === ticket;
      try {
        const location = await locate(sessionId);
        if (!location || !current()) return;
        ticket.exchange = track(
          launch(
            {
              ...fields,
              message: fields.message ?? location.title,
              ...order,
              event,
              session_id: sessionId,
              request_id: requestId,
              workspace_root: location.directory,
            },
            true,
          ),
        );
        if (!ticket.exchange) return;
        const { child } = ticket.exchange;
        child.once("exit", () => {
          if (!accepted) ticket.controller.abort();
        });
        const abort = () => child.stdin.end();
        ticket.controller.signal.addEventListener("abort", abort, {
          once: true,
        });
        try {
          packet = await firstPacket(child.stdout);
          if (
            !validPacket(event, packet) ||
            !current() ||
            child.exitCode !== null
          )
            return;
          const destination = await locate(sessionId);
          if (
            !destination ||
            destination.directory !== location.directory ||
            !current()
          )
            return;
          ticket.applying = true;
          await deliver(packet, ticket.controller.signal);
          accepted = true;
        } finally {
          ticket.controller.signal.removeEventListener("abort", abort);
        }
      } catch {
        if (packet)
          console.error(
            "Sona could not confirm delivery. Check the request in OpenCode.",
          );
      } finally {
        if (active.get(key) === ticket) active.delete(key);
        if (ticket.exchange) {
          ticket.exchange.child.stdin.end(JSON.stringify({ accepted }) + "\n");
          await ticket.exchange.closed;
        }
      }
    },

    cancelInput(sessionId) {
      for (const ticket of active.values()) {
        if (ticket.sessionId === sessionId && ticket.event === "input_request")
          cancel(ticket);
      }
    },

    cancelSession(sessionId) {
      for (const ticket of active.values()) {
        if (ticket.sessionId === sessionId) cancel(ticket);
      }
    },

    cancelRequest(sessionId, requestId) {
      for (const ticket of active.values()) {
        if (ticket.sessionId === sessionId && ticket.requestId === requestId)
          cancel(ticket);
      }
    },

    async close() {
      disposed = true;
      for (const ticket of active.values()) cancel(ticket, true);
      for (const exchange of children) exchange.child.stdin.end();
      await Promise.all([...children].map((exchange) => exchange.closed));
    },
  };
}
