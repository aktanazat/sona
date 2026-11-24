import { constants } from "node:fs";
import { open } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join } from "node:path";
import { z } from "zod";

const registration = z.object({
  pid: z.number().int(),
  url: z.string(),
  password: z.string().optional(),
});

const MAX_REGISTRATION_BYTES = 64 * 1024;

async function connection() {
  const state =
    process.env.XDG_STATE_HOME || join(homedir(), ".local", "state");
  if (!isAbsolute(state))
    throw new Error("OpenCode's state directory is not absolute");
  const file = await open(
    join(state, "opencode", "service.json"),
    constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK,
  );
  let info;
  try {
    const stat = await file.stat();
    if (
      !stat.isFile() ||
      stat.uid !== process.getuid() ||
      stat.nlink !== 1 ||
      (stat.mode & 0o077) !== 0 ||
      stat.size > MAX_REGISTRATION_BYTES
    ) {
      throw new Error("OpenCode's service registration is not private");
    }
    const bytes = Buffer.alloc(stat.size + 1);
    const { bytesRead } = await file.read(bytes, 0, bytes.length, 0);
    if (bytesRead !== stat.size)
      throw new Error("OpenCode's service registration changed");
    info = registration.parse(
      JSON.parse(bytes.subarray(0, bytesRead).toString("utf8")),
    );
  } finally {
    await file.close();
  }
  if (info.pid !== process.pid) {
    throw new Error(
      "OpenCode's service registration belongs to another process",
    );
  }
  const url = new URL(info.url);
  if (
    (url.protocol !== "http:" && url.protocol !== "https:") ||
    (url.hostname !== "127.0.0.1" && url.hostname !== "[::1]") ||
    url.username ||
    url.password ||
    url.pathname !== "/" ||
    url.search ||
    url.hash
  ) {
    throw new Error("OpenCode's service endpoint is not a local root URL");
  }
  const headers = { "Content-Type": "application/json" };
  if (info.password) {
    headers.Authorization =
      "Basic " + Buffer.from("opencode:" + info.password).toString("base64");
  }
  return { url, headers };
}

async function sendForm(sessionId, formId, body, signal) {
  const { url, headers } = await connection();
  const path = `/api/session/${encodeURIComponent(sessionId)}/form/${encodeURIComponent(formId)}`;
  const endpoint = new URL(body === undefined ? path : path + "/reply", url);
  const options = { headers, redirect: "error", signal };
  const response = await fetch(
    endpoint,
    body === undefined
      ? { ...options, method: "DELETE" }
      : { ...options, method: "POST", body },
  );
  await response.body?.cancel();
  if (!response.ok) throw new Error("OpenCode rejected the form response");
}

export function replyForm(sessionId, formId, answer, signal) {
  return sendForm(sessionId, formId, JSON.stringify({ answer }), signal);
}

export function rejectForm(sessionId, formId, signal) {
  return sendForm(sessionId, formId, undefined, signal);
}
