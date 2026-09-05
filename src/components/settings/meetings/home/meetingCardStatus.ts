import type { MeetingPhase, ProcessingStatus } from "@/bindings";

export type MeetingCardStatus =
  | "live"
  | "processing"
  | "ready"
  | "needs_attention";

/**
 * One meeting's state, read from its phase and its recorded processing status
 * and from nothing else.
 *
 * A non-terminal status is never evidence that work is happening: `pending` is
 * what a meeting is born with and what it keeps when the launch that was
 * processing it ends, which is how interrupted meetings used to read
 * "Processing" for days. So the only thing a status decides here is whether
 * processing ended badly; everything else is the phase, which is the one owner
 * of whether a job exists. Startup reconciliation is what makes that reading
 * true: it moves an abandoned meeting to `recovery_required` and gives it a
 * terminal status in the same transaction.
 *
 * The phase mapping is exhaustive on purpose. A catch-all returning
 * "processing" is the shape of the original bug — anything unrecognised read
 * as work in flight — so a new phase has to be classified here rather than
 * inheriting that answer.
 *
 * Round 7 took the last chip off the rows: only `needs_attention` prints a
 * word now, because it is the only state that asks the reader for something.
 * "Live", "Processing" and "Ready" were the machinery reporting on itself, so
 * this function's remaining job is to decide whether a row speaks at all.
 */
export const meetingCardStatus = (
  phase: MeetingPhase,
  processing: ProcessingStatus,
): MeetingCardStatus => {
  if (processing.kind === "failed" || processing.kind === "cancelled") {
    return "needs_attention";
  }
  switch (phase) {
    case "starting":
    case "capturing_recording":
    case "capturing_pausing":
    case "capturing_paused":
    case "capturing_resuming":
      return "live";
    case "preflight":
    case "stopping":
    case "processing":
    case "deleting":
      return "processing";
    case "review_ready":
      return "ready";
    case "recovery_required":
      return "needs_attention";
  }
};
