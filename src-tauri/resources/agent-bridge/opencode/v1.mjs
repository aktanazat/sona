import { randomUUID } from "node:crypto";
import { realpath } from "node:fs/promises";
import { isAbsolute } from "node:path";
import { z } from "zod";
import { assertAccepted, createBridge } from "./bridge.mjs";
import { legacyQuestions, checkedAnswers } from "./questions.mjs";

const sessionLocation = z.object({
  directory: z.string(),
  title: z.string().nullish(),
  parentID: z.string().nullish(),
});
const eventSession = z.object({ sessionID: z.string().min(1) });
const permissionRequest = z.object({ id: z.string(), permission: z.string() });
const questionRequest = z.object({ id: z.string() });

export async function server({ client, directory }) {
  const ownDirectory = await realpath(directory);
  const bridge = createBridge();
  const phases = new Map();

  const locate = async (sessionId) => {
    const result = await client.session.get({ path: { id: sessionId } });
    assertAccepted(result);
    const parsed = sessionLocation.safeParse(result.data);
    if (!parsed.success) return undefined;
    const info = parsed.data;
    if (!isAbsolute(info.directory)) return undefined;
    const actual = await realpath(info.directory);
    if (actual !== ownDirectory) return undefined;
    return { directory: actual, title: info.title, parentID: info.parentID };
  };

  const background = (operation) => {
    void operation.catch(() =>
      console.error("Sona could not read this OpenCode session."),
    );
  };

  const ready = (sessionId, message) => {
    if (phases.get(sessionId) === "idle") return;
    phases.set(sessionId, "idle");
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
            await client.session.promptAsync({
              path: { id: sessionId },
              body: { parts: [{ type: "text", text: packet.reason }] },
              signal,
            }),
          );
        },
      ),
    );
  };

  return {
    "chat.message": async (input, output) => {
      const sessionId = input.sessionID;
      phases.set(sessionId, "busy");
      bridge.cancelSession(sessionId);
      const prompt = output.parts
        .filter((part) => part.type === "text")
        .map((part) => part.text)
        .join("\n");
      background(
        bridge.publish(sessionId, "user_prompt_submit", { prompt }, locate),
      );
    },

    event: async ({ event }) => {
      const properties = event.properties;
      if (event.type === "server.instance.disposed") {
        await bridge.close();
        phases.clear();
        return;
      }
      if (event.type === "session.created") {
        background(
          bridge.publish(properties.info.id, "session_start", {}, locate),
        );
        return;
      }
      if (event.type === "session.deleted") {
        bridge.cancelSession(properties.info.id);
        phases.delete(properties.info.id);
        return;
      }
      const session = eventSession.safeParse(properties);
      if (!session.success) return;
      const sessionId = session.data.sessionID;
      if (
        event.type === "session.status" &&
        properties.status.type !== "idle"
      ) {
        phases.set(sessionId, "busy");
        bridge.cancelSession(sessionId);
      } else if (event.type === "session.idle") {
        ready(sessionId);
      } else if (event.type === "session.error") {
        ready(sessionId, properties.error?.data?.message);
      } else if (event.type === "permission.asked") {
        if (!permissionRequest.safeParse(properties).success) return;
        background(
          bridge.request(
            sessionId,
            properties.id,
            "permission_request",
            {
              tool_name: properties.permission,
              tool_input: {
                patterns: properties.patterns,
                metadata: properties.metadata,
              },
            },
            locate,
            async (packet, signal) => {
              assertAccepted(
                await client._client.post({
                  url: `/permission/${encodeURIComponent(properties.id)}/reply`,
                  headers: { "Content-Type": "application/json" },
                  body: {
                    reply: packet.decision === "allow" ? "once" : "reject",
                  },
                  signal,
                }),
              );
            },
          ),
        );
      } else if (event.type === "question.asked") {
        const questions = legacyQuestions(properties.questions);
        if (!questionRequest.safeParse(properties).success || !questions)
          return;
        background(
          bridge.request(
            sessionId,
            properties.id,
            "question_request",
            {
              tool_input: { questions },
            },
            locate,
            async (packet, signal) => {
              if (packet.decision === "deny") {
                assertAccepted(
                  await client._client.post({
                    url: `/question/${encodeURIComponent(properties.id)}/reject`,
                    signal,
                  }),
                );
                return;
              }
              checkedAnswers(questions, packet.answers);
              assertAccepted(
                await client._client.post({
                  url: `/question/${encodeURIComponent(properties.id)}/reply`,
                  headers: { "Content-Type": "application/json" },
                  body: { answers: packet.answers },
                  signal,
                }),
              );
            },
          ),
        );
      } else if (
        event.type === "question.replied" ||
        event.type === "question.rejected" ||
        event.type === "permission.replied"
      ) {
        bridge.cancelRequest(sessionId, properties.requestID);
      }
    },
  };
}
