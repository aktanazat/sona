import { useCallback, useEffect, useState } from "react";
import {
  commands,
  events,
  type LearningDecisionRequest,
  type LearningDecisionStatus,
} from "@/bindings";

/** Which candidate a row stands for: the decision request minus the answer. */
export type LearningCandidate = Omit<LearningDecisionRequest, "status">;

interface LearningDecisions<T> {
  /** What the surface should render right now, or null until the first read
   * answers: an empty list and an unread list are not the same claim. */
  entries: readonly T[] | null;
  /** Re-reads the list, for a surface with its own reason to refresh. */
  refresh: () => Promise<void>;
  /** Records an answer, then shows what is left. */
  decide: (
    candidate: LearningCandidate,
    status: LearningDecisionStatus,
  ) => Promise<void>;
}

/**
 * The one accept/dismiss flow for every surface that shows learning
 * suggestions.
 *
 * Surfaces differ in what they list and in how a row names its candidate.
 * Neither of those is what an *answer* does: the answer goes to the store,
 * which is where decision memory lives for all five loops, and the list is then
 * whatever the store still offers. That part lives here so a new suggestion row
 * cannot grow a second version of it, and so no surface has to remember that
 * the store — not the client — is what silences a candidate.
 *
 * `load` is re-read after every answer rather than trusting an echoed list:
 * one contract for both surfaces, and the lists it serves are capped in the
 * tens of rows.
 */
export const useLearningDecisions = <T>(
  load: () => Promise<readonly T[]>,
): LearningDecisions<T> => {
  const [entries, setEntries] = useState<readonly T[] | null>(null);

  const refresh = useCallback(async () => {
    try {
      setEntries(await load());
    } catch {
      /* A read that threw has answered as far as these surfaces go: a list
       * nobody could fetch offers nothing to decide. Leaving it unanswered
       * would hold the section blank for the rest of the session, and drop
       * the rejection on the floor besides. */
      setEntries([]);
    }
  }, [load]);

  useEffect(() => {
    void refresh();
    const subscription = events.meetingArtifactChanged.listen(
      () => void refresh(),
    );
    return () => {
      void subscription.then((unlisten) => unlisten());
    };
  }, [refresh]);

  const decide = useCallback(
    async (candidate: LearningCandidate, status: LearningDecisionStatus) => {
      await commands.learningDecide({ ...candidate, status });
      await refresh();
    },
    [refresh],
  );

  return { entries, refresh, decide };
};
