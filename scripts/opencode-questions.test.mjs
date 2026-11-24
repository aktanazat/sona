import { expect, test } from "bun:test";
import { validPacket } from "../src-tauri/resources/agent-bridge/opencode/bridge.mjs";
import { questionForm } from "../src-tauri/resources/agent-bridge/opencode/questions.mjs";

test("OpenCode free-text questions accept a typed answer", () => {
  const form = questionForm({
    metadata: { kind: "question" },
    fields: [
      {
        key: "note",
        title: "Reply",
        description: "What should I send?",
        required: true,
        type: "string",
      },
    ],
  });
  expect(form).toBeDefined();
  expect(form.questions[0]).toMatchObject({ custom: true, required: true });
  const text = " A typed answer with exact spacing. ";
  expect(form.answer([[text]]).note).toBe(text);
  expect(() => form.answer([[]])).toThrow();
});

test("Skipping optional free text does not insert its default", () => {
  const form = questionForm({
    metadata: { kind: "question" },
    fields: [
      {
        key: "note",
        title: "Additional context",
        required: false,
        type: "string",
        default: "Do not send this unless selected",
      },
    ],
  });
  expect(form).toBeDefined();
  expect(Object.hasOwn(form.answer([[]]), "note")).toBe(false);
});

test("OpenCode accepts only the exact native decision contract", () => {
  expect(
    validPacket("input_request", { decision: "block", reason: "Exact reply." }),
  ).toBe(true);
  expect(validPacket("permission_request", { decision: "allow" })).toBe(true);
  expect(validPacket("question_request", { answers: [["Keep"]] })).toBe(true);
  expect(validPacket("input_request", { decision: "block", reason: " " })).toBe(
    false,
  );
  expect(
    validPacket("permission_request", { decision: "allow", persistent: true }),
  ).toBe(false);
  expect(validPacket("question_request", { answers: [[4]] })).toBe(false);
});
