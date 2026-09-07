import { useEffect } from "react";
import { create } from "zustand";
import {
  commands,
  type PromptRun,
  type PromptRunFailure,
  type PromptTarget,
  type PromptTargetRef,
} from "@/bindings";

/* What ⌘K's prompt rows are about, and the one request that opens the editor.
 *
 * The palette is mounted with the shell and knows nothing about which record is
 * open, so the record says so itself: the meeting review screen and the person
 * page declare their noun for as long as they are on screen, and the palette
 * offers prompts only while there is one. A prop threaded from `App.tsx` would
 * have to travel through four components that have no use for it.
 *
 * `newPromptRequest` is a nonce rather than a boolean because asking twice has
 * to open the editor twice — the same reason the shell's own deep-link requests
 * carry one. */

interface PromptShellState {
  target: PromptTargetRef | null;
  newPromptRequest: number;
  handledNewPromptRequest: number;
  setTarget: (target: PromptTargetRef | null) => void;
  requestNewPrompt: () => void;
  consumeNewPromptRequest: (request: number) => boolean;
}

export const usePromptShellStore = create<PromptShellState>()((set) => ({
  target: null,
  newPromptRequest: 0,
  handledNewPromptRequest: 0,
  setTarget: (target) => set({ target }),
  requestNewPrompt: () =>
    set((state) => ({ newPromptRequest: state.newPromptRequest + 1 })),
  consumeNewPromptRequest: (request) => {
    let consumed = false;
    set((state) => {
      if (
        request === 0 ||
        request !== state.newPromptRequest ||
        request <= state.handledNewPromptRequest
      ) {
        return state;
      }
      consumed = true;
      return { handledNewPromptRequest: request };
    });
    return consumed;
  },
}));

/** The wire shape for one noun, from the two primitives a surface has. */
export const promptTargetRef = (
  kind: PromptTarget,
  id: string,
): PromptTargetRef =>
  kind === "meeting"
    ? { kind: "meeting", session_id: id }
    : kind === "person"
      ? { kind: "person", person_id: id }
      : { kind: "series", series_key: id };

/**
 * Declare what the open surface is about, for as long as it is open.
 *
 * Primitives rather than the built ref: a fresh object every render would
 * re-run the effect on every paint, and the two things that actually identify a
 * noun are its kind and its id.
 */
export const useOpenPromptTarget = (kind: PromptTarget, id: string): void => {
  const setTarget = usePromptShellStore((state) => state.setTarget);
  useEffect(() => {
    setTarget(promptTargetRef(kind, id));
    return () => setTarget(null);
  }, [kind, id, setTarget]);
};

/** Why a run produced nothing, as a sentence. */
export const promptFailureKeys = {
  model_unavailable: "prompts.failure.modelUnavailable",
  model_unreachable: "prompts.failure.modelUnreachable",
  model_failed: "prompts.failure.modelFailed",
  schema_mismatch: "prompts.failure.schemaMismatch",
  no_evidence: "prompts.failure.noEvidence",
} as const satisfies Record<PromptRunFailure, string>;

/** What each noun is called where a prompt says which one it is written for. */
export const promptTargetKeys = {
  meeting: "prompts.target.meeting",
  person: "prompts.target.person",
  series: "prompts.target.series",
} as const satisfies Record<PromptTarget, string>;

export type PromptRunOutcome =
  | { status: "run"; run: PromptRun }
  /** Nothing to ask about: a deleted prompt, or a noun with no meetings. */
  | { status: "missing" }
  | { status: "failed" };

/**
 * Ask one prompt, and hand back the run.
 *
 * A run that produced no answer is still a run: the surface shows the reason
 * beside it rather than treating it as a press that did nothing. Only the two
 * outcomes where nothing was written come back as anything else.
 */
export const runSavedPrompt = async (
  promptId: string,
  target: PromptTargetRef,
): Promise<PromptRunOutcome> => {
  try {
    const result = await commands.savedPromptRun({
      prompt_id: promptId,
      target,
    });
    if (result.status === "error") {
      return { status: result.error === "not_found" ? "missing" : "failed" };
    }
    return { status: "run", run: result.data };
  } catch {
    return { status: "failed" };
  }
};
