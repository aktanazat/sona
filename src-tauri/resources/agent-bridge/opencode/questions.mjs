import { z } from "zod";

const legacyPrompt = z.array(
  z.object({
    header: z.string().catch(""),
    question: z.string(),
    options: z.array(
      z.object({
        label: z.string(),
        description: z.string().nullable().catch(null),
      }),
    ),
    multiple: z.boolean().catch(false),
    custom: z.boolean().catch(true),
  }),
);
const answerBatch = z.array(z.array(z.string()));
const formChoice = z.object({
  label: z.string(),
  value: z.string(),
  description: z.string().nullable().catch(null),
});
const formPrompt = z.object({
  metadata: z.object({ kind: z.literal("question") }),
  fields: z
    .array(
      z.object({
        key: z.string().min(1),
        title: z.string().catch(""),
        description: z.string().catch(""),
        type: z.enum(["string", "boolean", "multiselect"]),
        options: z.array(formChoice).optional(),
        required: z.boolean().catch(false),
        custom: z.boolean().catch(false),
        hidden: z.literal(false).optional(),
        when: z.tuple([]).optional(),
      }),
    )
    .min(1),
});

function usable(questions) {
  return (
    questions.length > 0 &&
    questions.every(
      (question) =>
        question.question.trim().length > 0 &&
        (question.custom || question.options.length > 0) &&
        question.options.every(
          (option, index) =>
            option.label.trim().length > 0 &&
            !question.options
              .slice(0, index)
              .some((previous) => previous.label === option.label),
        ),
    )
  );
}

export function legacyQuestions(input) {
  const parsed = legacyPrompt.safeParse(input);
  if (!parsed.success) return undefined;
  const questions = parsed.data.map((question) => ({
    header: question.header,
    question: question.question,
    options: question.options.map((option) => ({
      label: option.label,
      description: option.description,
    })),
    multi_select: question.multiple === true,
    custom: question.custom !== false,
    required: true,
  }));
  return usable(questions) ? questions : undefined;
}

export function checkedAnswers(questions, answers) {
  if (
    !answerBatch.safeParse(answers).success ||
    answers.length !== questions.length ||
    answers.some((answer, index) => {
      const question = questions[index];
      return (
        (question.required && answer.length === 0) ||
        (!question.multi_select && answer.length > 1) ||
        answer.some(
          (selected, position) =>
            !selected.trim() ||
            answer.indexOf(selected) !== position ||
            (!question.custom &&
              !question.options.some((option) => option.label === selected)),
        )
      );
    })
  ) {
    throw new Error("The response does not match this OpenCode question");
  }
}

export function questionForm(form) {
  const parsed = formPrompt.safeParse(form);
  if (!parsed.success) return undefined;
  const questions = [];
  const mappings = [];
  const keys = new Set();
  for (const field of parsed.data.fields) {
    if (keys.has(field.key)) return undefined;
    keys.add(field.key);
    const question = field.description.trim() ? field.description : field.title;
    const header = field.title !== question ? field.title : "";
    const freeText = field.type === "string" && field.options === undefined;
    let options;
    let mapping;
    if (freeText) {
      options = [];
    } else if (field.type === "boolean") {
      options = [
        { label: "Yes", description: null },
        { label: "No", description: null },
      ];
      mapping = new Map([
        ["Yes", true],
        ["No", false],
      ]);
    } else if (field.type === "string" || field.type === "multiselect") {
      if (!field.options?.length) return undefined;
      options = field.options.map((option) => ({
        label: option.label,
        description: option.description,
      }));
      mapping = new Map(
        field.options.map((option) => [option.label, option.value]),
      );
    } else {
      return undefined;
    }
    questions.push({
      header,
      question,
      options,
      multi_select: field.type === "multiselect",
      custom: freeText || (field.type !== "boolean" && field.custom === true),
      required: field.required === true,
    });
    mappings.push({ key: field.key, type: field.type, values: mapping });
  }
  if (!usable(questions)) return undefined;
  return {
    questions,
    answer(selected) {
      checkedAnswers(questions, selected);
      const answer = Object.create(null);
      selected.forEach((choices, index) => {
        const mapping = mappings[index];
        if (choices.length === 0) return;
        const values = choices.map((choice) =>
          mapping.values?.has(choice) ? mapping.values.get(choice) : choice,
        );
        answer[mapping.key] =
          mapping.type === "multiselect" ? values : values[0];
      });
      return answer;
    },
  };
}
