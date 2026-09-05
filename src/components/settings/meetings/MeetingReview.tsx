import React, { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { ChevronLeft, MoreHorizontal } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  commands,
  type ManualNote,
  type MeetingExportFormat,
  type MeetingLoopRow,
  type MeetingReviewSnapshot,
  type MeetingSearchHit,
  type OperationReceipt,
  type SpeakerId,
} from "@/bindings";
import { Notice, PageTitle, SettingsPage } from "@/components/settings/rows";
import { Button } from "@/components/vg/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/vg/dropdown-menu";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/vg/tabs";
import { CloudMeetingActions } from "../../cloud-sync/CloudMeetingActions";
import type { SegmentJump } from "./review/Citations";
import { InsightsTab } from "./review/InsightsTab";
import { TranscriptTab } from "./review/TranscriptTab";
import { SpeakerIdentityDialog } from "./review/SpeakerIdentityDialog";
import { FollowUpAgentItem } from "./review/FollowUpAgentAction";
import { FollowUpDraftDialog } from "./review/FollowUpDraftAction";
import { useVoiceIdentity } from "./review/useVoiceIdentity";
import { MeetingLedgerSection } from "./MeetingLedgerSection";
import { currentLedger } from "./meetingLedger";
import { type LoopChange } from "./review/LoopRows";
import { committedEdit, inlineEditKeys } from "./review/inlineEdit";
import {
  formatMeetingDate,
  formatMeetingOffset,
  meetingErrorKey,
  meetingReasonKey,
  sourceKey,
} from "./meetingUtils";
import {
  actionItemKey,
  getMeetingAnalytics,
  setActionItemDone,
  type MeetingAnalyticsSnapshot,
} from "./meetingAnalytics";
import { useOpenPromptTarget } from "./promptTargets";

/* The review surface holds four jobs: read the transcript, fix it, read what
 * was generated from it, and get the record out. The first three are tabs,
 * because a person doing one of them is never doing the others at the same
 * time; the fourth is a verb like every other verb on this page, so it lives
 * with them in the one menu on the title line. Asking the meeting a question
 * is the chat column's job, with every meeting in reach rather than one.
 *
 * A citation is a jump: every generated claim and search hit that points at
 * a transcript segment scrolls that segment into view and marks it, which is
 * the whole reason citations exist. */

const REVIEW_TAB_IDS = ["transcript", "insights", "ledger"] as const;

type ReviewTab = (typeof REVIEW_TAB_IDS)[number];

/**
 * Which tab this record opens on, given whatever was decided before and the
 * snapshot in hand. `null` means nobody has decided yet — the record carries
 * nothing written about it, so the transcript is all there is to read.
 *
 * The ledger is the default reading of a finished meeting: where did we land,
 * and what was left open. A record that carries one opens there; a record
 * with notes but no ledger opens on Insights, which is still something to
 * read. The plain summary and the action list stay on Insights either way —
 * the ledger sits beside them, never in their place.
 *
 * A decision, once made, is never revisited: `chosen` short-circuits, so a
 * refresh cannot move a reader who picked a tab and a deleted last note cannot
 * pull one off Insights. The only transition this makes is the first one, from
 * "nothing to read" to "something to read", which is exactly the moment a
 * meeting finishes processing under an open review screen.
 */
export const nextReviewTab = (
  chosen: ReviewTab | null,
  snapshot: MeetingReviewSnapshot,
): ReviewTab | null =>
  chosen ??
  (currentLedger(snapshot.artifacts) !== null
    ? "ledger"
    : snapshot.artifacts.length > 0 || snapshot.notes.length > 0
      ? "insights"
      : null);

/* One row of text tabs. The kit's `line` variant draws the mark; this quiets
 * the type to the page's own 14px, weights the open one, and replaces the kit's
 * 2px black rule with the 1px accent the rest of the app underlines with. The
 * `group-data` prefix on the bar is not decoration: a plain `after:h-px` loses
 * to the kit's own variant selector. */
const TAB_TRIGGER_CLASSES =
  "flex-none px-0 text-[14px] leading-[21px] font-normal text-gray-900 transition-colors hover:text-gray-1000 focus-visible:border-transparent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none motion-reduce:transition-none data-[state=active]:font-medium data-[state=active]:text-gray-1000 after:bg-primary group-data-[orientation=horizontal]/tabs:after:h-px";

/** Every review panel is a column of sections on the page's own rhythm, set
 * off from the tab strip it opened under. */
const TAB_PANEL_CLASSES = "mt-6 flex flex-col gap-8";

/** How long typing settles before the store is asked for its own answer.
 * Short enough that a finished word is answered, long enough that a typed
 * word is one query rather than five. */
const SEARCH_SETTLE_MS = 250;

interface MeetingReviewProps {
  snapshot: MeetingReviewSnapshot;
  lastReceipt: OperationReceipt | null;
  pendingAction: string | null;
  onBack: () => void;
  onTitleSet: (title: string) => void;
  onSpeakerRename: (speakerId: SpeakerId, displayName: string) => void;
  onSpeakerMerge: (
    sourceSpeakerId: SpeakerId,
    targetSpeakerId: SpeakerId,
  ) => void;
  onSegmentEdit: (
    segmentId: string,
    replacementText: string,
    removed: boolean,
  ) => void;
  onNoteCreate: (body: string) => void;
  onNoteUpdate: (note: ManualNote, body: string) => void;
  onNoteDelete: (note: ManualNote) => void;
  onRegenerate: () => void;
  onExport: (format: MeetingExportFormat) => void;
  onRemoteCancel: () => void;
  onDelete: () => void;
  onRefresh: () => Promise<void>;
  onOpenSettings: () => void;
}

export const MeetingReview: React.FC<MeetingReviewProps> = ({
  snapshot,
  lastReceipt,
  pendingAction,
  onBack,
  onTitleSet,
  onSpeakerRename,
  onSpeakerMerge,
  onSegmentEdit,
  onNoteCreate,
  onNoteUpdate,
  onNoteDelete,
  onRegenerate,
  onExport,
  onRemoteCancel,
  onDelete,
  onRefresh,
  onOpenSettings,
}) => {
  const { t } = useTranslation();
  /* D19 gives a finished meeting a title from its own content, and what is
   * written about it — the generated notes, or the ones the reader typed
   * during it — is what somebody came back for; the transcript is the evidence
   * behind them, one click away.
   *
   * Decided once, and not before there is anything to decide from. This used
   * to be a plain `useState` initialiser over `snapshot.artifacts`, which
   * reads the record exactly once: a meeting whose artifacts landed after this
   * screen did — the ordinary case, since processing finishes while you are
   * looking at it — latched "transcript" on its first, empty snapshot and
   * stayed there for the rest of the visit. `null` is "nobody has decided
   * yet", which renders the transcript, so nothing flickers while the record
   * is still arriving. Once anything is decided it is final: a later snapshot
   * cannot move a reader who chose a tab, and deleting the last note cannot
   * pull one back off Insights. */
  const [chosenTab, setChosenTab] = useState<ReviewTab | null>(() =>
    nextReviewTab(null, snapshot),
  );
  useEffect(() => {
    setChosenTab((current) => nextReviewTab(current, snapshot));
  }, [snapshot]);
  const tab = chosenTab ?? "transcript";
  const [jump, setJump] = useState<SegmentJump | null>(null);
  const [newNote, setNewNote] = useState("");
  const [searchQuery, setSearchQuery] = useState("");
  const [searchHits, setSearchHits] = useState<MeetingSearchHit[] | null>(null);
  const [exportingLedger, setExportingLedger] = useState(false);
  const [analytics, setAnalytics] = useState<MeetingAnalyticsSnapshot | null>(
    null,
  );
  const [loops, setLoops] = useState<MeetingLoopRow[] | null>(null);
  const [loopsBusy, setLoopsBusy] = useState(false);
  const editable = snapshot.session.allowed_actions.includes("edit");
  const canRegenerate = snapshot.session.allowed_actions.includes("regenerate");
  const canExport =
    snapshot.can_export && snapshot.session.allowed_actions.includes("export");
  const canDelete = snapshot.session.allowed_actions.includes("delete");
  const canCancelRemote =
    snapshot.remote_cancellation_pending &&
    snapshot.session.allowed_actions.includes("cancel_remote");
  const speakerNames: Record<string, string> = {};
  for (const speaker of snapshot.speakers) {
    speakerNames[speaker.speaker_id] = speaker.display_name;
  }

  /* Metrics are derived from the transcript, so they are re-read whenever the
   * transcript could have moved: a new session revision means an edit, a
   * regeneration or a finished processing pass. */
  const sessionId = snapshot.session.session_id;
  const revision = snapshot.session.revision;
  const voiceIdentity = useVoiceIdentity({ snapshot, onRefresh });
  const busy = pendingAction !== null || voiceIdentity.pending;
  /* ⌘K offers this meeting's saved prompts for as long as it is on screen. */
  useOpenPromptTarget("meeting", sessionId);
  const loadAnalytics = useCallback(async () => {
    try {
      setAnalytics(await getMeetingAnalytics(sessionId));
    } catch {
      setAnalytics(null);
    }
  }, [sessionId]);

  const analyticsRevision = useRef<number | null>(null);
  useEffect(() => {
    if (analyticsRevision.current === revision) return;
    analyticsRevision.current = revision;
    void loadAnalytics();
  }, [loadAnalytics, revision]);

  const doneActionItems = new Set(
    (analytics?.action_items ?? [])
      .filter((state) => state.done)
      .map((state) => actionItemKey(state.artifact_id, state.action_index)),
  );

  const toggleActionItem = async (
    artifactId: string,
    actionIndex: number,
    done: boolean,
  ) => {
    try {
      const states = await setActionItemDone({
        session_id: sessionId,
        artifact_id: artifactId,
        action_index: actionIndex,
        done,
      });
      setAnalytics((current) =>
        current === null ? current : { ...current, action_items: states },
      );
    } catch {
      toast.error(t("meetings.errors.operation"));
    }
  };

  /* Loops are the ledger's actionable half: words from the artifact, state
   * from the store. Re-read on the same trigger as the metrics, because a
   * regeneration can rewrite the rows a resolution was attached to. */
  const loadLoops = useCallback(async () => {
    const loopsResult = await commands.meetingLoops(sessionId);
    setLoops(loopsResult.status === "ok" ? loopsResult.data.rows : []);
  }, [sessionId]);

  const loopsRevision = useRef<number | null>(null);
  useEffect(() => {
    if (loopsRevision.current === revision) return;
    loopsRevision.current = revision;
    void loadLoops();
  }, [loadLoops, revision]);

  /* One path for all three loop commands. They differ only in the request they
   * build, and each answers with the whole refreshed list, so the row somebody
   * just ticked never has to be patched by hand. */
  const changeLoop = async (row: MeetingLoopRow, change: LoopChange) => {
    const operation_id = crypto.randomUUID();
    const loop_id = row.loop_id;
    const expected_revision = row.revision;
    setLoopsBusy(true);
    try {
      const result = await (change.kind === "resolve"
        ? commands.meetingLoopResolve({
            operation_id,
            loop_id,
            expected_revision,
            resolution: change.dropped ? "dropped" : "done",
          })
        : change.kind === "reopen"
          ? commands.meetingLoopReopen({
              operation_id,
              loop_id,
              expected_revision,
            })
          : commands.meetingLoopAssign({
              operation_id,
              loop_id,
              expected_revision,
              owner_person_id: change.personId,
            }));
      if (result.status === "error") {
        toast.error(t(meetingErrorKey(result.error)));
        await loadLoops();
        return;
      }
      setLoops(result.data.loops.rows);
      // A refused write means somebody else moved this row first; the fresh
      // list that came back with the refusal is already the truth.
      if (result.data.receipt.reason_codes.includes("stale_revision")) {
        toast.info(t("meetings.loops.stale"));
      }
    } catch {
      toast.error(t("meetings.errors.operation"));
    } finally {
      setLoopsBusy(false);
    }
  };

  const selectTab = (id: string) => {
    const next = REVIEW_TAB_IDS.find((candidate) => candidate === id);
    if (next) setChosenTab(next);
  };

  const jumpToSegment = (segmentId: string) => {
    setChosenTab("transcript");
    /* A live filter that does not contain the cited turn would swallow the
     * jump, and a citation that lands nowhere is the one thing citations
     * cannot do — so arriving clears the filter. */
    setSearchQuery("");
    setJump((current) => ({ segmentId, nonce: (current?.nonce ?? 0) + 1 }));
  };

  const createNote = () => {
    const body = newNote.trim();
    if (body.length === 0) return;
    onNoteCreate(body);
    setNewNote("");
  };

  /* The field narrows the transcript as it is typed; the store is asked once
   * the typing settles, because its index is the authority on what this
   * meeting contains and reaches the notes and the title as well. */
  const searchTranscript = useCallback(
    async (query: string) => {
      const result = await commands.meetingSearch({
        query,
        session_ids: [sessionId],
        limit: 50,
      });
      if (result.status === "error") {
        toast.error(t(meetingErrorKey(result.error)));
        return null;
      }
      return result.data.entries;
    },
    [sessionId, t],
  );

  useEffect(() => {
    /* Hits belong to the query that asked for them, so a keystroke retires
     * them: the transcript narrows on the words alone until the store answers
     * the query now in the field. */
    setSearchHits(null);
    const query = searchQuery.trim();
    if (query.length === 0) return;

    /* A slow answer to an abandoned query must not overwrite a newer one. */
    let live = true;
    const timer = window.setTimeout(() => {
      searchTranscript(query)
        .then((hits) => {
          if (live && hits !== null) setSearchHits(hits);
        })
        .catch(() => {
          if (live) toast.error(t("meetings.errors.operation"));
        });
    }, SEARCH_SETTLE_MS);
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [searchQuery, searchTranscript, t]);

  /* The ledger page is written from an already-recorded revision, so it takes
   * no operation id and no expected revision: it mutates nothing. A cancelled
   * save dialog is the person changing their mind, not a failure, so it says
   * nothing. */
  const exportLedger = async () => {
    setExportingLedger(true);
    try {
      const result = await commands.produceLedgerHtml(
        snapshot.session.session_id,
      );
      if (result.status === "error") {
        // A cancelled save dialog is the person changing their mind.
        if (result.error === "export_cancelled") return;
        toast.error(
          result.error === "not_found"
            ? t("meetings.ledger.exportMissing")
            : t(meetingErrorKey(result.error)),
        );
        return;
      }
      toast.success(t("meetings.ledger.exported", { path: result.data }));
    } catch {
      toast.error(t("meetings.errors.operation"));
    } finally {
      setExportingLedger(false);
    }
  };

  const tabLabels = {
    transcript: t("meetings.review.tabs.transcript", "Transcript"),
    insights: t("meetings.review.tabs.insights", "Insights"),
    ledger: t("meetings.review.tabs.ledger", "Ledger"),
  } satisfies Record<ReviewTab, string>;

  return (
    /* The same column the settings pages are set in — a meeting is read at
     * the page's own measure. The title is an editable field rather than a
     * string, so it goes in the page's `header` slot. */
    <SettingsPage
      header={
        <MeetingReviewHeader
          snapshot={snapshot}
          lastReceipt={lastReceipt}
          busy={busy}
          editable={editable}
          canRegenerate={canRegenerate}
          canExport={canExport}
          canDelete={canDelete}
          hasLedger={currentLedger(snapshot.artifacts) !== null}
          unresolvedSpeakerCount={voiceIdentity.unresolvedSpeakerCount}
          onBack={onBack}
          onTitleSet={onTitleSet}
          onLabelSpeaker={voiceIdentity.openNextUnresolved}
          onRegenerate={onRegenerate}
          onExport={onExport}
          onExportLedger={() => void exportLedger()}
          onDelete={onDelete}
        />
      }
    >
      {snapshot.remote_cancellation_pending ? (
        <div className="flex flex-wrap items-center justify-between gap-3">
          <Notice tone="warning">
            {t("meetings.review.remoteCancellationPending")}
          </Notice>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={onRemoteCancel}
            disabled={busy || !canCancelRemote}
          >
            {t("meetings.review.cancelRemote")}
          </Button>
        </div>
      ) : null}

      <Tabs value={tab} onValueChange={selectTab} className="gap-0">
        <div className="-mx-8 border-b border-gray-alpha-400 px-8">
          <TabsList
            variant="line"
            aria-label={t(
              "meetings.review.tabsLabel",
              "Meeting review sections",
            )}
            className="justify-start gap-6 px-0"
          >
            {REVIEW_TAB_IDS.map((id) => (
              <TabsTrigger key={id} value={id} className={TAB_TRIGGER_CLASSES}>
                {tabLabels[id]}
              </TabsTrigger>
            ))}
          </TabsList>
        </div>

        <TabsContent value="transcript" className={TAB_PANEL_CLASSES}>
          <TranscriptTab
            snapshot={snapshot}
            speakerNames={speakerNames}
            busy={busy}
            editable={editable}
            jump={jump}
            searchQuery={searchQuery}
            searchHits={searchHits}
            onSearchQueryChange={setSearchQuery}
            onSegmentEdit={onSegmentEdit}
            onSpeakerRename={onSpeakerRename}
            onSpeakerMerge={onSpeakerMerge}
            onSpeakerCorrect={voiceIdentity.openCorrection}
          />
        </TabsContent>

        <TabsContent value="insights" className={TAB_PANEL_CLASSES}>
          <InsightsTab
            snapshot={snapshot}
            busy={busy}
            editable={editable}
            newNote={newNote}
            analytics={analytics?.analytics ?? null}
            speakerNames={speakerNames}
            doneActionItems={doneActionItems}
            onNewNoteChange={setNewNote}
            onCreateNote={createNote}
            onNoteUpdate={onNoteUpdate}
            onNoteDelete={onNoteDelete}
            onJumpToSegment={jumpToSegment}
            onActionItemToggle={(artifactId, actionIndex, done) =>
              void toggleActionItem(artifactId, actionIndex, done)
            }
            onRefresh={onRefresh}
            onAnalyticsRefresh={loadAnalytics}
            onOpenSettings={onOpenSettings}
          />
        </TabsContent>

        <TabsContent value="ledger" className={TAB_PANEL_CLASSES}>
          <MeetingLedgerSection
            snapshot={snapshot}
            busy={busy || exportingLedger || loopsBusy || !editable}
            onJumpToSegment={jumpToSegment}
            loops={loops}
            people={voiceIdentity.people ?? []}
            onLoopChange={(row, change) => void changeLoop(row, change)}
          />
        </TabsContent>
      </Tabs>

      {/* Sharing a meeting is a task, not a setting, and not one of the verbs
       * the menu holds: it has state of its own — how many shares exist,
       * whether a retry is scheduled — and that state is what a reader opens
       * it for. One closed row at the foot of the page, with the fact on its
       * summary. */}
      <CloudMeetingActions sessionId={snapshot.session.session_id} />

      <SpeakerIdentityDialog
        open={voiceIdentity.dialog !== null}
        mode={voiceIdentity.dialog?.kind ?? "label"}
        speaker={voiceIdentity.dialogSpeaker}
        people={voiceIdentity.people}
        peopleLoading={voiceIdentity.peopleLoading}
        peopleLoadFailed={voiceIdentity.peopleLoadFailed}
        pending={voiceIdentity.pending}
        unknownConfirming={voiceIdentity.unknownConfirming}
        onOpenChange={(open) => {
          if (!open) voiceIdentity.closeDialog();
        }}
        onRetryPeople={() => void voiceIdentity.retryPeople()}
        onSave={voiceIdentity.saveLabel}
        onUnknownRequest={voiceIdentity.requestUnknown}
        onUnknownCancel={voiceIdentity.cancelUnknown}
        onSkip={voiceIdentity.skipSpeaker}
      />
    </SettingsPage>
  );
};

interface MeetingReviewHeaderProps {
  snapshot: MeetingReviewSnapshot;
  lastReceipt: OperationReceipt | null;
  busy: boolean;
  editable: boolean;
  canRegenerate: boolean;
  canExport: boolean;
  canDelete: boolean;
  /** Whether the current revision carries a ledger to draft or export from. */
  hasLedger: boolean;
  unresolvedSpeakerCount: number | null;
  onBack: () => void;
  onTitleSet: (title: string) => void;
  onLabelSpeaker: () => void;
  onRegenerate: () => void;
  onExport: (format: MeetingExportFormat) => void;
  onExportLedger: () => void;
  onDelete: () => void;
}

/* The head of a document: the way back, its name, one line of facts about it,
 * and one menu holding every verb. Round 7 took the state words off that line
 * — "Ready for review" beside a screen you are already reviewing is the
 * machine describing itself — and left the three facts a reader checks
 * against their own memory of the meeting: when it was, how long it ran, and
 * what recorded it. */
const MeetingReviewHeader: React.FC<MeetingReviewHeaderProps> = ({
  snapshot,
  lastReceipt,
  busy,
  editable,
  canRegenerate,
  canExport,
  canDelete,
  hasLedger,
  unresolvedSpeakerCount,
  onBack,
  onTitleSet,
  onLabelSpeaker,
  onRegenerate,
  onExport,
  onExportLedger,
  onDelete,
}) => {
  const { t } = useTranslation();
  /* Renaming is the menu's job to start and the title's job to do, so the
   * state that connects them is the head's. */
  const [renaming, setRenaming] = useState(false);
  const startedAtUtcMs = snapshot.session.started_at_utc_ms;
  const elapsedOffsetNs = snapshot.session.elapsed_offset_ns;
  /* What recorded this, from the sources that were actually there to record
   * it. `health` never leaves "starting" on a live track, so availability is
   * the field that answers the question; a source that was not available is
   * why the completeness word beside this says "Partial recording". */
  const sources = [
    ...new Set(
      snapshot.session.sources
        .filter((source) => source.availability === "available")
        .map((source) => t(sourceKey(source.source_kind))),
    ),
  ].join(", ");
  const metadata = [
    startedAtUtcMs === null
      ? t("meetings.review.noStartTime")
      : formatMeetingDate(startedAtUtcMs),
    elapsedOffsetNs === null ? null : formatMeetingOffset(elapsedOffsetNs),
    sources.length === 0 ? null : sources,
  ]
    .filter((fact): fact is string => fact !== null)
    .join(" · ");

  return (
    <header className="flex flex-col gap-3">
      {/* Quiet text, no box: the way back out of a document is not a control
       * competing with the document's own name, and it names where it goes. */}
      <Button
        type="button"
        variant="ghost"
        size="sm"
        className="-ms-2 self-start ps-1.5 text-gray-900"
        onClick={onBack}
      >
        <ChevronLeft aria-hidden="true" className="size-3.5" />
        {t("meetings.review.backToList")}
      </Button>
      {/* The title and the facts under it are one block, set tight: a
       * document's name and its date are read together. The menu is on the
       * title's own line, right-aligned, because it acts on the document. */}
      <div className="flex min-w-0 flex-col gap-1.5">
        <div className="flex items-start justify-between gap-3">
          <MeetingTitleEditor
            key={
              snapshot.session.session_id +
              ":" +
              snapshot.session.revision +
              ":" +
              snapshot.session.title
            }
            title={snapshot.session.title}
            disabled={busy || !editable}
            editing={renaming}
            onEditingChange={setRenaming}
            onTitleSet={onTitleSet}
          />
          <MeetingActionsMenu
            snapshot={snapshot}
            busy={busy}
            editable={editable}
            canRegenerate={canRegenerate}
            canExport={canExport}
            canDelete={canDelete}
            hasLedger={hasLedger}
            onRename={() => setRenaming(true)}
            onRegenerate={onRegenerate}
            onExport={onExport}
            onExportLedger={onExportLedger}
            onDelete={onDelete}
          />
        </div>
        {/* One line of facts about the recording. The completeness word only
         * appears when it changes what the record can be trusted for, and a
         * partial recording has to say so in words. */}
        <p className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
          <span className="text-[13px] leading-[18px] tabular-nums text-gray-900">
            {metadata}
          </span>
          {snapshot.session.capture_completeness === "partial" ? (
            <span className="text-[13px] leading-[18px] text-accent-strong">
              {t("meetings.review.partialRecording")}
            </span>
          ) : null}
        </p>
      </div>
      {/* Voices nobody has named yet: one quiet line and the one press that
       * answers it. The only verb left outside the menu, because it is the
       * answer to a question the line beside it just asked. */}
      {unresolvedSpeakerCount !== null && unresolvedSpeakerCount > 0 ? (
        <p className="flex flex-wrap items-center gap-3">
          <span className="text-[13px] leading-[18px] text-gray-900">
            {t("meetings.review.unresolvedSpeakers", {
              count: unresolvedSpeakerCount,
            })}
          </span>
          <Button
            type="button"
            size="sm"
            variant="outline"
            disabled={busy || !editable}
            onClick={onLabelSpeaker}
          >
            {t("meetings.review.labelSpeaker")}
          </Button>
        </p>
      ) : null}
      {lastReceipt?.session_id === snapshot.session.session_id ? (
        <MeetingReceipt receipt={lastReceipt} />
      ) : null}
    </header>
  );
};

interface MeetingActionsMenuProps {
  snapshot: MeetingReviewSnapshot;
  busy: boolean;
  editable: boolean;
  canRegenerate: boolean;
  canExport: boolean;
  canDelete: boolean;
  hasLedger: boolean;
  onRename: () => void;
  onRegenerate: () => void;
  onExport: (format: MeetingExportFormat) => void;
  onExportLedger: () => void;
  onDelete: () => void;
}

/* Every verb this page has, in one place. They used to be seven bordered
 * buttons parked on three reading surfaces — two on the ledger's label line,
 * two on the generated-notes line, three in an export bar under everything —
 * which put a row of controls between the reader and each thing they came to
 * read. A verb is not the page's subject, so none of them is on the page.
 *
 * Delete is last and separated, and it keeps the confirmation it always had:
 * the sheet cannot open from a mis-aimed press, and the dialogs live out here
 * rather than inside the menu, which unmounts its own children on select. */
const MeetingActionsMenu: React.FC<MeetingActionsMenuProps> = ({
  snapshot,
  busy,
  editable,
  canRegenerate,
  canExport,
  canDelete,
  hasLedger,
  onRename,
  onRegenerate,
  onExport,
  onExportLedger,
  onDelete,
}) => {
  const { t } = useTranslation();
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [draftOpen, setDraftOpen] = useState(false);
  /* Rename is the only verb here that takes the keyboard: it opens the title
   * field, which claims the caret as it mounts. An open menu traps focus, and
   * the trap outlives the press by the length of the menu's exit animation, so
   * a field opened on the press loses the caret to the menu that is leaving
   * and then to the menu's own focus restore. So the press records the verb
   * and the close runs it: by then the trap is gone, the restore is declined,
   * and the field is the only thing asking for the keyboard. Every other verb
   * leaves the keyboard alone and keeps the ordinary restore. */
  const renameOnClose = useRef(false);

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            className="flex-none text-gray-900"
            aria-label={t("common.more")}
          >
            <MoreHorizontal aria-hidden="true" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align="end"
          onCloseAutoFocus={(event) => {
            if (!renameOnClose.current) return;
            renameOnClose.current = false;
            event.preventDefault();
            onRename();
          }}
        >
          <DropdownMenuItem
            disabled={busy || !editable}
            onSelect={() => {
              renameOnClose.current = true;
            }}
          >
            {t("meetings.review.rename")}
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={busy || !canRegenerate}
            onSelect={onRegenerate}
          >
            {t("meetings.review.regenerate")}
          </DropdownMenuItem>
          {hasLedger ? (
            <DropdownMenuItem
              disabled={busy}
              onSelect={() => setDraftOpen(true)}
            >
              {t("meetings.followUp.draft")}
            </DropdownMenuItem>
          ) : null}
          <FollowUpAgentItem snapshot={snapshot} />
          <DropdownMenuItem
            disabled={busy || !canExport}
            onSelect={() => onExport("markdown")}
          >
            {t("meetings.review.exportMarkdown")}
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={busy || !canExport}
            onSelect={() => onExport("json")}
          >
            {t("meetings.review.exportJson")}
          </DropdownMenuItem>
          {hasLedger ? (
            <DropdownMenuItem
              disabled={busy || !canExport}
              onSelect={onExportLedger}
            >
              {t("meetings.ledger.exportHtml")}
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuSeparator />
          <DropdownMenuItem
            variant="destructive"
            disabled={busy || !canDelete}
            onSelect={() => setDeleteOpen(true)}
          >
            {t("meetings.actions.delete")}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>

      <FollowUpDraftDialog
        sessionId={snapshot.session.session_id}
        open={draftOpen}
        onOpenChange={setDraftOpen}
      />

      <Dialog open={deleteOpen} onOpenChange={setDeleteOpen}>
        <DialogContent showCloseButton={false}>
          <DialogHeader>
            <DialogTitle>{t("meetings.delete.title")}</DialogTitle>
            <DialogDescription>
              {t("meetings.delete.explainsData")}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setDeleteOpen(false)}
            >
              {t("common.cancel")}
            </Button>
            <Button
              type="button"
              variant="destructive"
              onClick={() => {
                setDeleteOpen(false);
                onDelete();
              }}
            >
              {t("meetings.actions.delete")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
};

interface MeetingTitleEditorProps {
  title: string;
  disabled: boolean;
  /** Open, because the words were pressed or Rename was chosen from the menu. */
  editing: boolean;
  onEditingChange: (editing: boolean) => void;
  onTitleSet: (title: string) => void;
}

/* The meeting's title is the page's title, so it is the page's title: an H1
 * that reads as one. D19 writes it from the meeting's own content, which makes
 * editing the exception rather than the expected first act — so there is no
 * field and no Save button until somebody asks for one. Two things ask: the
 * words themselves, and Rename in the menu, which is why the open state is
 * the head's rather than this component's. */
const MeetingTitleEditor: React.FC<MeetingTitleEditorProps> = ({
  title,
  disabled,
  editing,
  onEditingChange,
  onTitleSet,
}) => {
  const { t } = useTranslation();

  const commit = (draft: string) => {
    onEditingChange(false);
    const next = committedEdit(draft, title);
    if (next !== null) onTitleSet(next);
  };

  if (editing) {
    return (
      /* Set in the H1's own type so opening the field moves no other line on
       * the page; the heading is still the heading behind it. */
      <input
        autoFocus
        defaultValue={title}
        aria-label={t("meetings.review.meetingTitle")}
        onBlur={(event) => commit(event.target.value)}
        onKeyDown={inlineEditKeys(commit, () => onEditingChange(false))}
        className="w-full border-0 border-b border-ring bg-transparent pb-px text-[24px] leading-[30px] font-semibold tracking-[-0.01em] text-gray-1000 outline-none"
      />
    );
  }

  return (
    <PageTitle className="min-w-0">
      {disabled ? (
        title
      ) : (
        <button
          type="button"
          title={t("meetings.review.editTitle")}
          onClick={() => onEditingChange(true)}
          className="cursor-pointer rounded-md text-start transition-colors hover:text-gray-900 focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none motion-reduce:transition-none"
        >
          {title}
        </button>
      )}
    </PageTitle>
  );
};

interface MeetingReceiptProps {
  receipt: OperationReceipt;
}

const MeetingReceipt: React.FC<MeetingReceiptProps> = ({ receipt }) => {
  const { t } = useTranslation();
  const reasons = receipt.reason_codes
    .map((reason) => t(meetingReasonKey(reason)))
    .join(" · ");

  return (
    /* What the store wrote, in the words a person would use for it. The
     * "Correction receipt" label over it named the machinery, not the fact. */
    <p
      aria-live="polite"
      className="text-[13px] leading-[18px] tabular-nums text-gray-900"
    >
      {receipt.new_revision === null
        ? t("meetings.receipts.saved")
        : t("meetings.receipts.savedRevision", {
            revision: receipt.new_revision,
          })}
      {reasons.length > 0 ? ` ${reasons}` : ""}
    </p>
  );
};
