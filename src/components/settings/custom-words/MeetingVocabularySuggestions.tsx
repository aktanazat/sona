import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type LearningDecisionStatus,
  type VocabularyCandidate,
  type VocabularyEntry,
} from "@/bindings";
import { useLearningDecisions } from "@/hooks/useLearningDecisions";
import { spokenMatchKey } from "@/lib/vocabularyDraft";
import { Button } from "@/components/vg/button";
import { Microlabel } from "@/components/settings/rows";

interface MeetingVocabularySuggestionsProps {
  entries: readonly VocabularyEntry[];
  onAccept: (text: string) => void;
}

interface MeetingVocabularySuggestionsListProps {
  candidates: readonly VocabularyCandidate[];
  onAccept: (text: string) => void;
  onDismiss: (text: string) => void;
}

export const MeetingVocabularySuggestionsList: React.FC<
  MeetingVocabularySuggestionsListProps
> = ({ candidates, onAccept, onDismiss }) => {
  const { t } = useTranslation();
  if (candidates.length === 0) return null;

  return (
    <div
      data-testid="meeting-vocabulary-suggestions"
      className="space-y-2 border-b border-gray-alpha-400 px-6 py-3"
    >
      <Microlabel>
        {t("settings.workflows.vocabularySuggestions.title")}
      </Microlabel>
      <ul role="list" className="divide-y divide-gray-alpha-400">
        {candidates.map((candidate) => (
          <li
            key={candidate.text}
            className="flex min-w-0 flex-wrap items-center justify-between gap-3 py-2 first:pt-1 last:pb-0"
          >
            <div className="min-w-0">
              <p className="truncate text-[14px] text-gray-1000">
                {candidate.text}
              </p>
              <p className="text-[12px] text-gray-700 tabular-nums">
                {t("settings.workflows.vocabularySuggestions.occurrences", {
                  count: candidate.occurrences,
                })}{" "}
                ·{" "}
                {t("settings.workflows.vocabularySuggestions.meetings", {
                  count: candidate.meetings_count,
                })}
              </p>
            </div>
            <div className="flex items-center gap-1">
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => onAccept(candidate.text)}
              >
                {t("settings.workflows.vocabularySuggestions.accept")}
              </Button>
              <Button
                type="button"
                size="sm"
                variant="ghost"
                onClick={() => onDismiss(candidate.text)}
              >
                {t("settings.workflows.vocabularySuggestions.dismiss")}
              </Button>
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
};

const loadCandidates = async (): Promise<readonly VocabularyCandidate[]> => {
  try {
    const result = await commands.vocabularyCandidates();
    return result.status === "ok" ? result.data.entries : [];
  } catch {
    return [];
  }
};

export const MeetingVocabularySuggestions: React.FC<
  MeetingVocabularySuggestionsProps
> = ({ entries, onAccept }) => {
  /* An answer is remembered by the store, not by this device: the same
   * (loop, candidate) memory serves every learning loop, and the candidate list
   * already arrives with answered terms removed. */
  const { entries: candidates, decide } = useLearningDecisions(loadCandidates);

  const knownTerms = useMemo(() => {
    const terms = new Set<string>();
    for (const entry of entries) {
      terms.add(spokenMatchKey(entry.spoken));
      terms.add(spokenMatchKey(entry.written));
    }
    return terms;
  }, [entries]);

  /* An unread list and an empty list draw the same thing here - the list
   * itself is what this surface offers, and there is no sentence to take back. */
  const visibleCandidates = (candidates ?? []).filter(
    (candidate) => !knownTerms.has(spokenMatchKey(candidate.text)),
  );

  /* The mined term itself, never a rendered line: an accepted vocabulary
   * decision is what loop 4 primes a session's ASR with. */
  const answer = (text: string, status: LearningDecisionStatus) =>
    void decide(
      { loop_kind: "vocabulary_term", candidate_key: text, display_text: text },
      status,
    );

  return (
    <MeetingVocabularySuggestionsList
      candidates={visibleCandidates}
      onAccept={(text) => {
        onAccept(text);
        answer(text, "accepted");
      }}
      onDismiss={(text) => answer(text, "dismissed")}
    />
  );
};
