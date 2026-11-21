import { randomUUID } from "node:crypto";
import { realpath } from "node:fs/promises";
import { isAbsolute } from "node:path";
import { z } from "zod";
import { assertAccepted, createBridge } from "./bridge.mjs";
import { questionForm } from "./questions.mjs";
import { replyForm, rejectForm } from "./service.mjs";

const sessionLocation = z.object({
  location: z.object({
    directory: z.string(),
    workspaceID: z.string().nullish(),
  }),
  title: z.string().nullish(),
  parentID: z.string().nullish(),
});
const eventSession = z.object({ sessionID: z.string().min(1) });
const formLocation = z.object({ id: z.string(), sessionID: z.string() });
const permissionRequest = z.object({ id: z.string(), action: z.string() });

const EVENT_TYPES = [
  "session.created",
  "session.execution.started",
  "session.execution.succeeded",
  "session.execution.failed",
  "session.execution.interrupted",
  "session.moved",
  "session.deleted",
  "permission.asked",
  "permission.replied",
  "form.created",
  "form.replied",
  "form.cancelled",
];

export async function setup(ctx) {
  const directory = await realpath(ctx.location.directory);
  const bridge = createBridge();
  const controller = new AbortController();
  const locate = async (sessionId) => {
    const parsed = sessionLocation.safeParse(
      await ctx.session.get({ sessionID: sessionId }),
    );
    if (!parsed.success) return undefined;
    const info = parsed.data;
    if (
      !isAbsolute(info.location.directory) ||
      info.location.workspaceID !== ctx.location.workspaceID
    )
      return undefined;
    const actual = await realpath(info.location.directory);
    if (actual !== directory) return undefined;
    return { directory: actual, title: info.title, parentID: info.parentID };
  };
  const background = (operation) => {
    void operation.catch(() =>
      console.error("Sona could not read this OpenCode session."),
    );
  };
  const prompt = await ctx.session.hook("prompt", (event) => {
    bridge.cancelSession(event.sessionID);
    background(
      bridge.publish(
        event.sessionID,
        "user_prompt_submit",
        { prompt: event.prompt.text },
        locate,
      ),
    );
  });
  const ready = (sessionId, message) => {
    bridge.cancelInput(sessionId);
    background(
      bridge.request(
        sessionId,
        randomUUID(),
        "input_request",
        { message },
        async (id) => {
          const location = await locate(id);
          return location?.parentID ? undefined : location;
        },
        async (packet, signal) => {
          assertAccepted(
            await ctx.session.prompt(
              { sessionID: sessionId, text: packet.reason },
              { signal },
            ),
          );
        },
      ),
    );
  };
  const events = (async () => {
    try {
      for await (const event of ctx.event.subscribe({
        types: EVENT_TYPES,
        signal: controller.signal,
      })) {
        const data = event.data;
        if (event.type === "form.created") {
          const form = data.form;
          const normalized = questionForm(form);
          if (
            !normalized ||
            !formLocation.safeParse(form).success ||
            form.sessionID === "global"
          )
            continue;
          background(
            bridge.request(
              form.sessionID,
              form.id,
              "question_request",
              {
                message: form.title,
                tool_input: { questions: normalized.questions },
              },
              locate,
              async (packet, signal) => {
                if (packet.decision === "deny") {
                  await rejectForm(form.sessionID, form.id, signal);
                } else {
                  await replyForm(
                    form.sessionID,
                    form.id,
                    normalized.answer(packet.answers),
                    signal,
                  );
                }
              },
            ),
          );
          continue;
        }
        const session = eventSession.safeParse(data);
        if (!session.success) continue;
        const sessionId = session.data.sessionID;
        switch (event.type) {
          case "session.created":
            background(bridge.publish(sessionId, "session_start", {}, locate));
            break;
          case "session.execution.started":
          case "session.execution.interrupted":
          case "session.moved":
          case "session.deleted":
            bridge.cancelSession(sessionId);
            break;
          case "session.execution.succeeded":
            ready(sessionId);
            break;
          case "session.execution.failed":
            ready(sessionId, data.error?.message);
            break;
          case "permission.asked":
            if (!permissionRequest.safeParse(data).success) break;
            background(
              bridge.request(
                sessionId,
                data.id,
                "permission_request",
                {
                  message: data.message,
                  tool_name: data.action,
                  tool_input: {
                    resources: data.resources,
                    metadata: data.metadata,
                  },
                },
                locate,
                async (packet, signal) => {
                  assertAccepted(
                    await ctx.permission.reply(
                      {
                        sessionID: sessionId,
                        requestID: data.id,
                        decision:
                          packet.decision === "allow" ? "once" : "reject",
                      },
                      { signal },
                    ),
                  );
                },
              ),
            );
            break;
          case "permission.replied":
            bridge.cancelRequest(sessionId, data.requestID);
            break;
          case "form.replied":
          case "form.cancelled":
            bridge.cancelRequest(sessionId, data.id);
            break;
        }
      }
      if (!controller.signal.aborted) {
        console.error(
          "Sona stopped receiving OpenCode updates. Restart OpenCode to reconnect.",
        );
      }
    } catch {
      if (!controller.signal.aborted) {
        console.error(
          "Sona stopped receiving OpenCode updates. Restart OpenCode to reconnect.",
        );
      }
    } finally {
      await bridge.close();
    }
  })();
  return async () => {
    controller.abort();
    await bridge.close();
    await events;
    await prompt.dispose();
  };
}
