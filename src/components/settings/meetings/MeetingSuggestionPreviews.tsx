import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import type { MeetingSuggestion, SourceKind } from "@/bindings";
import { useSettingsStore } from "@/stores/settingsStore";
import {
  MeetingPreviewCard,
  MeetingPreviewList,
  suggestionFacts,
} from "./MeetingPreviewCard";

/* Offers raised by a running meeting application.
 *
 * The suggestion payload is content-free by design — provider, bundle id,
 * evidence flags, two instants — so these cards are short, and they are short
 * honestly: there is no time row because nothing scheduled the call, and no
 * participants row because no list exists to read. The rows that do appear
 * (the app, and what the next press will record) are the ones the operator can
 * still act on.
 *
 * The section carries no description: "Sona noticed a meeting app in use" was
 * the heading again in a longer form, and the card underneath already names
 * the app it noticed. Round 7 took the footnote about Skip with it — that an
 * offer expires on its own clock is true of every offer here, and a sentence
 * explaining a control the reader has not pressed is the page talking about
 * itself. Skip is local either way: no offer starts anything, and hiding one
 * changes nothing but this list.
 */

export interface MeetingSuggestionPreviewsProps {
  suggestions: MeetingSuggestion[];
  /** What the next press will record. Read-only here: the page has one
   *  answer to that question and Settings owns it. */
  sources: SourceKind[];
  starting: boolean;
  onStartSuggestion: (suggestion: MeetingSuggestion) => void;
}

export const MeetingSuggestionPreviews: React.FC<
  MeetingSuggestionPreviewsProps
> = ({ suggestions, sources, starting, onStartSuggestion }) => {
  const { t } = useTranslation();
  const [skipped, setSkipped] = useState<string[]>([]);
  const notesTemplate = useSettingsStore(
    (state) => state.settings?.meeting_notes_template ?? null,
  );

  const visible = suggestions.filter(
    (suggestion) => !skipped.includes(suggestion.offer_id),
  );
  if (visible.length === 0) return null;

  return (
    <MeetingPreviewList label={t("meetings.detected.title")}>
      {visible.map((suggestion) => (
        <MeetingPreviewCard
          key={suggestion.offer_id}
          facts={suggestionFacts(suggestion, t)}
          recording={{ armed: sources }}
          notesTemplate={notesTemplate}
          starting={starting}
          onStart={() => onStartSuggestion(suggestion)}
          onSkip={() => setSkipped([...skipped, suggestion.offer_id])}
        />
      ))}
    </MeetingPreviewList>
  );
};
