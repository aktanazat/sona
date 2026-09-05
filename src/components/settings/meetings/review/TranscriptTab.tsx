import React, { useEffect, useState } from "react";
import { Search } from "lucide-react";
import { useTranslation } from "react-i18next";
import type {
  EffectiveTranscriptSegment,
  MeetingReviewSnapshot,
  MeetingSearchHit,
  SpeakerId,
} from "@/bindings";
import { cn } from "@/lib/cn";
import {
  Microlabel,
  Notice,
  SettingsSection,
  SettingsSurface,
} from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Input } from "@/components/vg/input";
import { Textarea } from "@/components/vg/textarea";
import { MeetingSourceList, ProcessingStatusText } from "../MeetingStatus";
import { formatMeetingOffset } from "../meetingUtils";
import type { SegmentJump } from "./Citations";
import { GapRows } from "./GapTimeline";
import { committedEdit, inlineEditKeys } from "./inlineEdit";
import { SpeakerRoster } from "./SpeakerRoster";

/* The transcript is a document, not a form.
 *
 * What a meeting said is the thing a person came here to read, so a stretch
 * one voice held is set as one paragraph: the name once, the moment it started
 * in the gutter, and the sentences flowing after each other. One bordered row
 * per sentence — with the speaker's name repeated on every one and "Okay." in
 * a box of its own — is what this replaced; it made a record you could not
 * read out of a record you could not edit.
 *
 * Every sentence stays its own element underneath the prose: a citation
 * resolves one by DOM id, a jump lights exactly one, and pressing one opens
 * the field that corrects it. The apparatus for changing words, and the one
 * destructive action that belongs with it, exists only on the sentence
 * somebody is actually correcting and leaves when they are done. */

/** DOM id prefix for transcript sentences, so a citation can find its own. */
const SEGMENT_DOM_PREFIX = "meeting-transcript-segment-";

/** Meta: the clock reading in the gutter, and the facts beside a search hit. */
const META = "text-[13px] leading-[18px] text-gray-900";

/** The turn's own type. The field that corrects a sentence is set in it too,
 * so opening an editor moves nothing else on the page. */
const TURN_TEXT = "text-[14px] leading-[21px] text-pretty";

/** How long the sentence a jump landed on stays lit. Long enough to find with
 * your eye after the scroll, short enough that it does not become the
 * sentence's resting state. */
const FLASH_MS = 1_200;

/** Consecutive sentences one voice held the floor for. */
export interface TranscriptTurnModel {
  speakerId: string;
  segments: EffectiveTranscriptSegment[];
}

/**
 * The transcript as turns: every run of neighbouring sentences by one speaker
 * becomes one paragraph.
 *
 * `keep` decides which sentences the reader asked for, and it runs here rather
 * than before, because a filtered-out sentence has to break the run it sat in.
 * Grouping an already-filtered list would glue two remarks made minutes apart
 * into one paragraph under one timestamp.
 */
export const transcriptTurns = (
  segments: readonly EffectiveTranscriptSegment[],
  keep: (segment: EffectiveTranscriptSegment) => boolean,
): TranscriptTurnModel[] => {
  const turns: TranscriptTurnModel[] = [];
  let open: TranscriptTurnModel | null = null;
  for (const segment of segments) {
    if (!keep(segment)) {
      open = null;
      continue;
    }
    if (open !== null && open.speakerId === segment.assigned_speaker_id) {
      open.segments.push(segment);
      continue;
    }
    open = { speakerId: segment.assigned_speaker_id, segments: [segment] };
    turns.push(open);
  }
  return turns;
};

export interface TranscriptTabProps {
  snapshot: MeetingReviewSnapshot;
  speakerNames: Record<string, string>;
  busy: boolean;
  editable: boolean;
  jump: SegmentJump | null;
  searchQuery: string;
  searchHits: MeetingSearchHit[] | null;
  onSearchQueryChange: (value: string) => void;
  onSegmentEdit: (
    segmentId: string,
    replacementText: string,
    removed: boolean,
  ) => void;
  onSpeakerRename: (speakerId: SpeakerId, displayName: string) => void;
  onSpeakerMerge: (
    sourceSpeakerId: SpeakerId,
    targetSpeakerId: SpeakerId,
  ) => void;
  onSpeakerCorrect: (speakerId: SpeakerId) => void;
}

export const TranscriptTab: React.FC<TranscriptTabProps> = ({
  snapshot,
  speakerNames,
  busy,
  editable,
  jump,
  searchQuery,
  searchHits,
  onSearchQueryChange,
  onSegmentEdit,
  onSpeakerRename,
  onSpeakerMerge,
  onSpeakerCorrect,
}) => {
  const { t } = useTranslation();
  const disabled = busy || !editable;
  /* The nonce of the last jump whose flash has burned out. A fresh jump is lit
   * on the render that carries it, so the sentence is already marked when the
   * scroll arrives. */
  const [settledJump, setSettledJump] = useState<number | null>(null);
  /* One sentence is open at a time, so the list owns which one — and therefore
   * owns what opening and closing an editor mean. A turn holds no state, which
   * is why a jump or an edit can never leave two open. */
  const [editingSegmentId, setEditingSegmentId] = useState<string | null>(null);

  useEffect(() => {
    if (jump === null) return;
    /* A sentence that is not on screen has nothing to scroll to, but the flash
     * still has to burn out or it would arrive already lit. */
    document
      .getElementById(`${SEGMENT_DOM_PREFIX}${jump.segmentId}`)
      ?.scrollIntoView({
        block: "center",
        behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches
          ? "auto"
          : "smooth",
      });
    const timer = window.setTimeout(() => setSettledJump(jump.nonce), FLASH_MS);
    return () => window.clearTimeout(timer);
  }, [jump]);

  const commitSegment = (segmentId: string, current: string, draft: string) => {
    setEditingSegmentId(null);
    const next = committedEdit(draft, current);
    if (next !== null) onSegmentEdit(segmentId, next, false);
  };

  /* Typing narrows the transcript to the sentences that carry the words, and
   * the store's own answer is folded in beside them: its index matches what a
   * substring scan cannot, and it is the authority on what this meeting
   * contains. What it found outside the transcript has no sentence to become,
   * so it stays a fact about where the match lives. */
  const query = searchQuery.trim().toLocaleLowerCase();
  const hits = searchHits ?? [];
  const hitSegmentIds = new Set(
    hits.filter((hit) => hit.kind === "transcript").map((hit) => hit.entity_id),
  );
  const elsewhere = hits.filter((hit) => hit.kind !== "transcript");
  const turns = transcriptTurns(snapshot.transcript, (segment) =>
    query.length === 0
      ? true
      : hitSegmentIds.has(segment.base.segment_id) ||
        (segment.replacement_text ?? segment.base.text)
          .toLocaleLowerCase()
          .includes(query),
  );

  return (
    <>
      <SpeakerRoster
        speakers={snapshot.speakers}
        diarization={snapshot.diarization}
        disabled={disabled}
        onRename={onSpeakerRename}
        onMerge={onSpeakerMerge}
        onCorrect={onSpeakerCorrect}
      />

      {/* No heading over this: the tab reading "Transcript" already named it.
       * The search field keeps the line to itself. */}
      <section className="flex flex-col gap-2">
        <div className="flex min-h-5 items-center justify-end gap-4">
          <div className="relative min-w-40 flex-1 sm:max-w-72">
            <Search
              aria-hidden="true"
              className="pointer-events-none absolute start-2 top-1/2 size-3 -translate-y-1/2 text-gray-800"
            />
            <Input
              type="search"
              value={searchQuery}
              onChange={(event) => onSearchQueryChange(event.target.value)}
              onKeyDown={(event) => {
                if (event.key !== "Escape") return;
                event.preventDefault();
                onSearchQueryChange("");
              }}
              placeholder={t("meetings.review.searchPlaceholder")}
              aria-label={t("meetings.review.searchPlaceholder")}
              className="h-7 w-full ps-7 text-[14px] md:text-[14px]"
            />
          </div>
        </div>
        <SettingsSurface>
          {snapshot.transcript.length === 0 ? (
            <div className="px-6 py-3.5">
              <Notice tone="muted" live={false}>
                {t("meetings.review.noTranscript")}
              </Notice>
            </div>
          ) : (
            <>
              {elsewhere.length === 0 ? null : (
                <ul role="list" className="divide-y divide-gray-alpha-400">
                  {elsewhere.map((hit) => (
                    <li
                      key={`${hit.kind}:${hit.entity_id}`}
                      data-slot="search-hit-elsewhere"
                      className="flex flex-col gap-1 px-6 py-3.5"
                    >
                      <span className="flex items-baseline gap-2">
                        <span className={cn(META, "tabular-nums")}>
                          {formatMeetingOffset(hit.start_offset_ns)}
                        </span>
                        <Microlabel>
                          {hit.kind === "manual_note"
                            ? t("meetings.review.hitKind.manualNote")
                            : t("meetings.review.hitKind.title")}
                        </Microlabel>
                      </span>
                      <span className="line-clamp-2 block text-[14px] leading-[21px] text-gray-1000">
                        {hit.excerpt}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
              {turns.length > 0 ? (
                <ol
                  role="list"
                  aria-label={t("meetings.review.transcript")}
                  className="flex flex-col gap-5 px-6 py-5"
                >
                  {turns.map((turn) => {
                    const first = turn.segments[0].base;
                    return (
                      <TranscriptTurn
                        key={first.segment_id}
                        speaker={
                          speakerNames[turn.speakerId] ??
                          t("meetings.review.unknownSpeaker")
                        }
                        time={formatMeetingOffset(first.start_offset_ns)}
                        segments={turn.segments.map((segment) => {
                          const segmentId = segment.base.segment_id;
                          const landed = jump?.segmentId === segmentId;
                          return {
                            segmentId,
                            time: formatMeetingOffset(
                              segment.base.start_offset_ns,
                            ),
                            text: segment.replacement_text ?? segment.base.text,
                            removed: segment.removed,
                            landed,
                            flashing:
                              landed &&
                              jump !== null &&
                              settledJump !== jump.nonce,
                            editing: editingSegmentId === segmentId,
                          };
                        })}
                        query={query}
                        disabled={disabled}
                        onOpenEdit={setEditingSegmentId}
                        onCommit={commitSegment}
                        onCancel={() => setEditingSegmentId(null)}
                        onRemove={(segmentId, current) => {
                          setEditingSegmentId(null);
                          onSegmentEdit(segmentId, current, true);
                        }}
                      />
                    );
                  })}
                </ol>
              ) : elsewhere.length > 0 || searchHits === null ? null : (
                /* The store's verdict, not the substring scan's: while its
                 * answer to the query in the field is still on the way, the
                 * surface says nothing rather than something untrue. */
                <div className="px-6 py-3.5">
                  <Notice tone="muted" live={false}>
                    {t("meetings.review.noSearchResults")}
                  </Notice>
                </div>
              )}
            </>
          )}
        </SettingsSurface>
      </section>

      {/* How the audio came in, and where it did not. One section: a source
       * that lost audio and the moments it lost are the same fact, and the
       * second box used to say "No gaps detected" under a header that had
       * already said the recording was complete. */}
      <SettingsSection label={t("meetings.review.capture")}>
        <MeetingSourceList
          sources={snapshot.session.sources}
          label={t("meetings.review.capture")}
        />
        <GapRows gaps={snapshot.gaps} />
        {snapshot.session.processing_status.kind === "succeeded" ? null : (
          <div className="px-6 py-3.5">
            <ProcessingStatusText
              status={snapshot.session.processing_status}
              className="block"
            />
          </div>
        )}
      </SettingsSection>
    </>
  );
};

/** One sentence of a turn, as the list handed it over. */
export interface TranscriptTurnSegment {
  segmentId: string;
  /** Clock reading for the moment this sentence starts. */
  time: string;
  text: string;
  removed: boolean;
  /** The sentence a citation jumped to, still marked after its flash. */
  landed: boolean;
  flashing: boolean;
  editing: boolean;
}

export interface TranscriptTurnProps {
  speaker: string;
  /** Clock reading for the moment the turn starts, set in the gutter. */
  time: string;
  segments: TranscriptTurnSegment[];
  /** Lower-cased live filter, marked inside the words. Empty marks nothing. */
  query: string;
  disabled: boolean;
  onOpenEdit: (segmentId: string) => void;
  onCommit: (segmentId: string, current: string, draft: string) => void;
  onCancel: () => void;
  onRemove: (segmentId: string, current: string) => void;
}

/* One turn: the name, the moment, and the words.
 *
 * Every sentence is still its own element inside the paragraph — the id a
 * citation resolves, the mark a jump leaves, and the field that corrects it
 * all belong to the sentence rather than to the turn it flows in. They are
 * written here rather than in a component of their own because a turn is one
 * paragraph: splitting the sentence out put a component boundary in the
 * middle of a sentence's own line box, and there is nothing on either side of
 * it that the other half does not need.
 *
 * It owns no state — which sentence is open and what closing one means both
 * belong to the list — so it renders exactly what it is handed and can be
 * read, or rendered by a test, in either state without arranging a click
 * first. */
export const TranscriptTurn: React.FC<TranscriptTurnProps> = ({
  speaker,
  time,
  segments,
  query,
  disabled,
  onOpenEdit,
  onCommit,
  onCancel,
  onRemove,
}) => {
  const { t } = useTranslation();
  /* One sentence at a time is corrected, and its removal is offered under the
   * paragraph rather than inside it: a red word wedged between two sentences
   * read as part of what was said. */
  const correcting = segments.find((segment) => segment.editing) ?? null;

  return (
    <li data-slot="transcript-turn" className="flex gap-4">
      {/* The gutter: one clock reading for the turn, out of the prose so the
       * sentences read as sentences. */}
      <span className={cn(META, "w-11 flex-none pt-0.5 tabular-nums")}>
        {time}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <span className="text-[14px] leading-[21px] font-medium text-gray-1000">
          {speaker}
        </span>
        <p className={cn(TURN_TEXT, "text-gray-1000")}>
          {segments.map((segment, index) => {
            const { segmentId, text } = segment;
            const marks = cn(
              /* The flash marked the jumped-to sentence with
               * `bg-blue-alpha-200`, a token that does not exist, so what a
               * citation pointed at looked like every other sentence. The
               * soft accent is the token that does. */
              "rounded-sm transition-colors motion-reduce:transition-none",
              segment.flashing
                ? "bg-accent-soft"
                : segment.landed
                  ? "bg-gray-alpha-100"
                  : "",
              segment.removed ? "line-through" : "",
            );

            return (
              <React.Fragment key={segmentId}>
                {index === 0 ? null : " "}
                {segment.editing ? (
                  <Textarea
                    autoFocus
                    rows={1}
                    defaultValue={text}
                    aria-label={t("meetings.review.transcriptSegment")}
                    onBlur={(event) =>
                      onCommit(segmentId, text, event.target.value)
                    }
                    onKeyDown={inlineEditKeys(
                      (draft) => onCommit(segmentId, text, draft),
                      onCancel,
                    )}
                    className={cn(
                      TURN_TEXT,
                      "my-0.5 inline-block min-h-0 w-full resize-none rounded-none border-0 border-b border-ring px-0 py-0 align-bottom text-gray-1000 md:text-[14px]",
                    )}
                  />
                ) : disabled ? (
                  /* A transcript nobody may correct is text, with nothing to
                   * focus and nothing to press: what is left to say about a
                   * sentence is the moment it was said. */
                  <span
                    id={`${SEGMENT_DOM_PREFIX}${segmentId}`}
                    data-slot="transcript-segment"
                    title={segment.time}
                    className={marks}
                  >
                    <MarkedText text={text} query={query} />
                  </span>
                ) : (
                  <span
                    id={`${SEGMENT_DOM_PREFIX}${segmentId}`}
                    data-slot="transcript-segment"
                    role="button"
                    tabIndex={0}
                    title={t("meetings.review.editTurn")}
                    onClick={() => onOpenEdit(segmentId)}
                    onKeyDown={(event) => {
                      if (event.key !== "Enter" && event.key !== " ") return;
                      event.preventDefault();
                      onOpenEdit(segmentId);
                    }}
                    className={cn(
                      marks,
                      "cursor-pointer decoration-gray-alpha-500 decoration-dotted underline-offset-4 hover:underline focus-visible:underline",
                    )}
                  >
                    <MarkedText text={text} query={query} />
                  </span>
                )}
              </React.Fragment>
            );
          })}
        </p>
        {correcting === null || correcting.removed ? null : (
          /* The one destructive action this surface has, inside the state it
           * belongs to: you cannot remove a sentence you are not already
           * correcting, and reading the transcript never offers it.
           *
           * Keeping the field focused on the way down is what makes it
           * pressable at all: the blur that commits would otherwise close
           * this editor out from under the click. */
          <Button
            type="button"
            variant="link"
            size="xs"
            className="h-auto self-start px-0 text-[12px] font-normal text-red-900 hover:text-red-900"
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => onRemove(correcting.segmentId, correcting.text)}
          >
            {t("meetings.review.removeSegment")}
          </Button>
        )}
      </div>
    </li>
  );
};

interface MarkedTextProps {
  text: string;
  /** Already lower-cased by the caller, which also owns the empty case. */
  query: string;
}

/** The words, with every run the live filter matched marked in place. */
const MarkedText: React.FC<MarkedTextProps> = ({ text, query }) => {
  if (query.length === 0) return <>{text}</>;

  const haystack = text.toLocaleLowerCase();
  const parts: React.ReactNode[] = [];
  let cursor = 0;
  for (
    let at = haystack.indexOf(query);
    at !== -1;
    at = haystack.indexOf(query, cursor)
  ) {
    if (at > cursor) parts.push(text.slice(cursor, at));
    parts.push(
      <mark key={at} className="bg-accent-soft text-gray-1000">
        {text.slice(at, at + query.length)}
      </mark>,
    );
    cursor = at + query.length;
  }
  if (parts.length === 0) return <>{text}</>;
  if (cursor < text.length) parts.push(text.slice(cursor));

  return <>{parts}</>;
};
