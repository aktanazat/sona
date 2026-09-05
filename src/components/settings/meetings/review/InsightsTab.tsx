import React, { useState } from "react";
import { Plus } from "lucide-react";
import { useTranslation } from "react-i18next";
import type {
  EngineFailureCause,
  ManualNote,
  MeetingReviewSnapshot,
} from "@/bindings";
import { cn } from "@/lib/cn";
import { CardBand, CardFooterAction } from "@/components/settings/CardBand";
import {
  Microlabel,
  SETTINGS_CARD,
  SettingsSurface,
} from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import { Textarea } from "@/components/vg/textarea";
import { MeetingAnalyticsStrip } from "../MeetingAnalyticsStrip";
import { MeetingNotesPane } from "../MeetingNotesPane";
import type { MeetingAnalytics } from "../meetingAnalytics";
import { formatMeetingOffset, processingStatusKey } from "../meetingUtils";
import { MeetingArtifactPanel } from "./MeetingArtifactPanel";
import { PreviouslyTogetherBand } from "./PreviouslyTogetherBand";
import { PromptResults } from "./PromptResults";
import { committedEdit, inlineEditKeys } from "./inlineEdit";

/** A note as it reads, and the field that corrects one, set in the same type
 * so opening the editor moves nothing on the page. */
const NOTE_TEXT = "text-[14px] leading-[21px] text-pretty";

/** The line each way of refusing gets, and whether pressing it again can
 * change the answer.
 *
 * A cause the next run reproduces — a record that could not be read, a track
 * the detector refused, a pack that does not fit — names itself and offers
 * nothing: the same evidence reaches the same engine and ends the same way,
 * so a Regenerate link here would be a button that lies. The three that come
 * out of the model's own answer are worth pressing again, because the next
 * answer is a different one. */
const FAILURE_CAUSES = {
  storage: { line: "meetings.processing.cause.storage", retry: false },
  transcription: {
    line: "meetings.processing.cause.transcription",
    retry: false,
  },
  voice_detection: {
    line: "meetings.processing.cause.voice_detection",
    retry: false,
  },
  evidence_pack: {
    line: "meetings.processing.cause.evidence_pack",
    retry: false,
  },
  model_refused: {
    line: "meetings.processing.cause.model_refused",
    retry: true,
  },
  reply_not_structured: {
    line: "meetings.processing.cause.reply_not_structured",
    retry: true,
  },
  reply_rejected: {
    line: "meetings.processing.cause.reply_rejected",
    retry: true,
  },
  panicked: { line: "meetings.processing.cause.panicked", retry: false },
} satisfies Record<EngineFailureCause, { line: string; retry: boolean }>;

export interface InsightsTabProps {
  snapshot: MeetingReviewSnapshot;
  busy: boolean;
  editable: boolean;
  newNote: string;
  /** Conversation metrics, or null until the first read lands. */
  analytics: MeetingAnalytics | null;
  speakerNames: Record<string, string>;
  /** Ticked action items, keyed by artifact id and index. */
  doneActionItems: Set<string>;
  onNewNoteChange: (value: string) => void;
  onCreateNote: () => void;
  onNoteUpdate: (note: ManualNote, body: string) => void;
  onNoteDelete: (note: ManualNote) => void;
  onJumpToSegment: (segmentId: string) => void;
  onActionItemToggle: (
    artifactId: string,
    actionIndex: number,
    done: boolean,
  ) => void;
  onRefresh: () => Promise<void>;
  onAnalyticsRefresh: () => Promise<void>;
  onOpenSettings: () => void;
  /** Runs the generation again. Offered only where a second run can answer
   * differently; see [`FAILURE_CAUSES`]. */
  onRegenerate: () => void;
}

export const InsightsTab: React.FC<InsightsTabProps> = ({
  snapshot,
  busy,
  editable,
  newNote,
  analytics,
  speakerNames,
  doneActionItems,
  onNewNoteChange,
  onCreateNote,
  onNoteUpdate,
  onNoteDelete,
  onJumpToSegment,
  onActionItemToggle,
  onRefresh,
  onAnalyticsRefresh,
  onOpenSettings,
  onRegenerate,
}) => {
  const { t } = useTranslation();
  const disabled = busy || !editable;
  const processingStatus = snapshot.session.processing_status;
  const processing =
    processingStatus.kind === "pending" || processingStatus.kind === "running";
  /* Why there are no notes, when the answer is a failure rather than a wait.
   *
   * This used to name exactly one reason, `remote_unavailable`, on the
   * argument that every remote destination resolves to it today. True along
   * that axis, and the collision was on the other one: `LocalModelUnavailable`
   * is the reason `generation_shortfall` calls "the case that filled the
   * corpus with blank meetings", and it rendered as "No generated notes …
   * they can be rebuilt at any time" — the same screen as a meeting that
   * recorded silence and has nothing to rebuild from. So the reason is read
   * off the status instead of being matched against one value, and
   * `processingStatusKey` already has a word for all five.
   *
   * `cancelled` is somebody stopping the pass, not a fault, so it names
   * itself without the alarm the other four earn. */
  const failure =
    processingStatus.kind === "failed" ? processingStatus.reason : null;
  const hasSettingsRecovery =
    failure === "local_model_unavailable" || failure === "remote_unavailable";
  /* Rows written before the cause existed carry nothing, and every reason
   * other than `engine_failure` already names itself completely. Both read as
   * the one sentence this line has always been. */
  const causeCopy =
    processingStatus.kind === "failed" && processingStatus.cause
      ? FAILURE_CAUSES[processingStatus.cause]
      : null;
  /* Retry is offered where a second run can answer differently and the
   * meeting still takes one. A closed row lists no `regenerate`, and a link
   * that calls a command the store refuses is a link that lies. */
  const canRetry =
    causeCopy?.retry === true &&
    snapshot.session.allowed_actions.includes("regenerate");

  return (
    <>
      <PreviouslyTogetherBand sessionId={snapshot.session.session_id} />

      {analytics === null ? null : (
        <MeetingAnalyticsStrip
          analytics={analytics}
          speakerNames={speakerNames}
          onJumpToSegment={onJumpToSegment}
        />
      )}

      {/* One card for everything somebody wrote here by hand. A cream band
       * names it, the notes read as the notes they are — a stamp and a
       * sentence per row — and the two things you can do to them sit at the
       * bottom edge as offers rather than as controls parked on a reading
       * surface: add a note, or open the field that steers the next pass.
       *
       * The field on a note, and the one destructive action that belongs with
       * it, exist only on the note being changed. This used to be two cards
       * and a section heading: an always-open "My notes" text area above an
       * empty text area above an Add button above the notes themselves. */}
      <div className={cn(SETTINGS_CARD, "overflow-hidden")}>
        <CardBand as="h2" title={t("meetings.review.notesTitle")} />
        {snapshot.notes.length === 0 ? null : (
          <ul
            role="list"
            aria-label={t("meetings.review.manualNotes")}
            className="divide-y divide-gray-alpha-400"
          >
            {snapshot.notes.map((note) => (
              <ManualNoteRow
                key={`${note.note_id}:${note.revision}`}
                note={note}
                disabled={disabled}
                onUpdate={onNoteUpdate}
                onDelete={onNoteDelete}
              />
            ))}
          </ul>
        )}
        {editable ? (
          <NoteComposer
            value={newNote}
            disabled={disabled}
            onChange={onNewNoteChange}
            onCommit={onCreateNote}
          />
        ) : null}
        {/* At rest one more line at the same edge; opened, the band-and-body
         * the steer field has always been. It mounts either way, because
         * whether it has anything in it is a fact only it can read. */}
        <MeetingNotesPane
          sessionId={snapshot.session.session_id}
          revision={snapshot.session.revision}
          variant="review"
          disabled={busy}
          onEnhanced={async () => {
            await onRefresh();
            await onAnalyticsRefresh();
          }}
        />
      </div>

      {/* No label over this surface: the tab reading "Insights" already names
       * what these are, and the verbs that used to sit on that label line —
       * Regenerate, and asking the meeting a question — are rows of the
       * page's one menu now. */}
      <SettingsSurface>
        {snapshot.artifacts.length === 0 ? (
          /* One line for the whole empty case: what would fill this, and how.
           * A failure names itself in red on that same line, because it is
           * the only version of this the reader has to act on, and it stays
           * announced — a failure arriving under an open review screen is
           * worth saying out loud; "nothing here yet" is not. */
          <p
            role={failure === null ? undefined : "status"}
            aria-live={failure === null ? undefined : "polite"}
            className="flex flex-wrap items-baseline gap-x-1.5 gap-y-1 px-6 py-5 text-[13px] leading-5 text-gray-800"
          >
            {failure === null ? null : (
              <span
                className={failure === "cancelled" ? undefined : "text-red-900"}
              >
                {t(processingStatusKey(processingStatus))}
              </span>
            )}
            <span>
              {processing
                ? t("meetings.review.processingDescription")
                : failure === null
                  ? t("meetings.review.noGeneratedNotesDescription")
                  : causeCopy === null
                    ? t("meetings.review.generationFailedDescription")
                    : t(causeCopy.line)}
            </span>
            {hasSettingsRecovery ? (
              <Button variant="link" size="xs" onClick={onOpenSettings}>
                {t("chat.openSettings")}
              </Button>
            ) : null}
            {canRetry ? (
              <Button
                variant="link"
                size="xs"
                disabled={disabled}
                onClick={onRegenerate}
              >
                {t("meetings.review.regenerate")}
              </Button>
            ) : null}
            {processing ? (
              <Button
                variant="link"
                size="xs"
                disabled={busy}
                onClick={() => void onRefresh()}
              >
                {t("meetings.actions.refresh")}
              </Button>
            ) : null}
          </p>
        ) : (
          snapshot.artifacts.map((artifact) => (
            <MeetingArtifactPanel
              key={artifact.artifact_id}
              artifact={artifact}
              doneActionItems={doneActionItems}
              actionsDisabled={busy}
              onJump={onJumpToSegment}
              onActionItemToggle={onActionItemToggle}
            />
          ))
        )}
      </SettingsSurface>

      <PromptResults kind="meeting" id={snapshot.session.session_id} />
    </>
  );
};

interface ManualNoteRowProps {
  note: ManualNote;
  disabled: boolean;
  onUpdate: (note: ManualNote, body: string) => void;
  onDelete: (note: ManualNote) => void;
}

/* One note: when it was said, and what was said. Pressing the words opens the
 * field on them; Enter and blur commit, Escape and an emptied field leave the
 * note as it was. */
const ManualNoteRow: React.FC<ManualNoteRowProps> = ({
  note,
  disabled,
  onUpdate,
  onDelete,
}) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);

  const commit = (draft: string) => {
    setEditing(false);
    const next = committedEdit(draft, note.body);
    if (next !== null) onUpdate(note, next);
  };

  return (
    <li className="flex flex-col gap-1 px-6 py-3.5">
      {/* When in the meeting, in the same words the citation marks use: the
       * bare time. "Timestamp 0:34" said the machine's name for the number
       * beside it, and a note nobody stamped does not need a line saying so. */}
      {note.start_offset_ns === null ? null : (
        <Microlabel className="tabular-nums">
          {formatMeetingOffset(note.start_offset_ns)}
        </Microlabel>
      )}
      {editing ? (
        <>
          <Textarea
            autoFocus
            rows={2}
            defaultValue={note.body}
            aria-label={t("meetings.review.manualNote")}
            onBlur={(event) => commit(event.target.value)}
            onKeyDown={inlineEditKeys(commit, () => setEditing(false))}
            className={cn(
              NOTE_TEXT,
              "min-h-0 resize-none rounded-none border-0 border-b border-gray-alpha-400 px-0 py-0 text-gray-1000 md:text-[14px]",
            )}
          />
          {/* The one destructive action this section has, inside the state it
           * belongs to. `onMouseDown` keeps the field's blur from closing the
           * editor out from under the press. */}
          <Button
            type="button"
            variant="link"
            size="xs"
            className="h-auto self-start px-0 text-[12px] font-normal text-red-900 hover:text-red-900"
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => onDelete(note)}
          >
            {t("common.delete")}
          </Button>
        </>
      ) : disabled ? (
        <p className={cn(NOTE_TEXT, "text-gray-1000")}>{note.body}</p>
      ) : (
        <button
          type="button"
          onClick={() => setEditing(true)}
          title={t("meetings.review.editNote")}
          className={cn(
            NOTE_TEXT,
            "-mx-1.5 cursor-pointer rounded-md px-1.5 text-start text-gray-1000 transition-colors hover:bg-gray-alpha-100 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none motion-reduce:transition-none",
          )}
        >
          {note.body}
        </button>
      )}
    </li>
  );
};

interface NoteComposerProps {
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
  onCommit: () => void;
}

/* Where a note is added: one quiet line at the card's bottom edge, which
 * becomes the field when somebody means to type. It leaves the way it came —
 * Enter or blur writes the note, Escape and an empty field just close it — so
 * the card is never a form waiting to be filled in. */
const NoteComposer: React.FC<NoteComposerProps> = ({
  value,
  disabled,
  onChange,
  onCommit,
}) => {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);

  const close = () => {
    setOpen(false);
    onChange("");
  };

  const commit = (draft: string) => {
    setOpen(false);
    if (draft.trim().length === 0) {
      onChange("");
      return;
    }
    onCommit();
  };

  if (!open) {
    return (
      <CardFooterAction disabled={disabled} onClick={() => setOpen(true)}>
        <Plus aria-hidden="true" />
        {t("meetings.review.addNote")}
      </CardFooterAction>
    );
  }

  return (
    <div className="border-t border-gray-alpha-400 px-6 py-3.5">
      <Textarea
        autoFocus
        rows={2}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={t("meetings.review.notePlaceholder")}
        aria-label={t("meetings.review.newNote")}
        disabled={disabled}
        onBlur={(event) => commit(event.target.value)}
        onKeyDown={inlineEditKeys(commit, close)}
        className={cn(
          NOTE_TEXT,
          "min-h-0 resize-none rounded-none border-0 border-b border-gray-alpha-400 px-0 py-0 text-gray-1000 md:text-[14px]",
        )}
      />
    </div>
  );
};
