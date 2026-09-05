import { useTranslation } from "react-i18next";
import { useMeetingImport } from "@/hooks/useMeetingImport";
import { eventFacts, suggestionFacts } from "../MeetingPreviewCard";
import type {
  MeetingsController,
  MeetingsSettingsProps,
} from "../meetingTypes";
import { isActiveMeetingPhase, isPreflightMeetingPhase } from "../meetingUtils";
import { useMeetingActionBindings } from "./useMeetingActionBindings";
import { useMeetingHomeActions } from "./useMeetingHomeActions";
import { useMeetingMutations } from "./useMeetingMutations";
import { useMeetingOutcomes } from "./useMeetingOutcomes";
import { useMeetingSnapshotReader } from "./useMeetingSnapshotReader";
import { useMeetingStartFlow } from "./useMeetingStartFlow";
import { useMeetingStartSetup } from "./useMeetingStartSetup";
import { useMeetingWorkflow } from "./useMeetingWorkflow";
import { useMeetingsFeed } from "./useMeetingsFeed";

/** Composes independent reads and command owners into one screen-specific
 * contract. The returned union always has three members: screen, model, and
 * actions. */
export const useMeetingsController = ({
  invalidation = 0,
  navigationRequest = null,
  startRequest = 0,
}: MeetingsSettingsProps): MeetingsController => {
  const { t } = useTranslation();
  const outcomes = useMeetingOutcomes();
  const setup = useMeetingStartSetup();
  const feed = useMeetingsFeed();
  const readMeeting = useMeetingSnapshotReader(outcomes.reportMeetingError);
  const workflow = useMeetingWorkflow({
    invalidation,
    navigationRequest,
    readMeeting,
    refreshHome: feed.refreshHome,
    startOptions: setup.startOptions,
  });
  const startFlow = useMeetingStartFlow({
    workflow,
    refreshHome: feed.refreshHome,
    receiveReceipt: outcomes.receiveReceipt,
    reportMeetingError: outcomes.reportMeetingError,
  });
  const mutations = useMeetingMutations({
    transitions: workflow.transitions,
    refreshHome: feed.refreshHome,
    receiveReceipt: outcomes.receiveReceipt,
    reportMeetingError: outcomes.reportMeetingError,
  });
  const actionBindings = useMeetingActionBindings(mutations);
  const meetingImport = useMeetingImport({
    onImported: (sessionId) => {
      void workflow.transitions.openSession(sessionId);
      void feed.refreshHome();
    },
  });
  const homeActions = useMeetingHomeActions({
    transitions: workflow.transitions,
    readMeeting,
    mutations,
  });
  const { screen, snapshot, pendingAction } = workflow.state;

  if (screen.kind === "home") {
    return {
      screen: "home",
      model: {
        suggestions: feed.suggestions,
        recovery: feed.recovery,
        meetings: feed.meetings,
        loading: feed.listLoading && feed.meetings.length === 0,
        paging: feed.listLoading,
        hasMore: feed.hasMore,
        page: feed.page,
        filter: feed.filter,
        error: feed.homeError,
        sources: setup.sources,
        starting: pendingAction === "start",
        focusStart: startRequest > 0,
      },
      actions: {
        onStart: () => {
          void startFlow.startMeeting(setup.startOptions("manual"));
        },
        onImport: () => {
          void meetingImport.start();
        },
        onStartSuggestion: (suggestion) => {
          void startFlow.startMeeting(
            setup.startOptions(
              "suggestion",
              suggestion.offer_id,
              undefined,
              suggestionFacts(suggestion, t),
            ),
          );
        },
        onStartEvent: (event) => {
          const facts = eventFacts(event, t);
          void startFlow.startMeeting(
            setup.startOptions(
              "manual",
              null,
              facts.title,
              facts,
              event.eventKey,
            ),
          );
        },
        onOpenMeeting: (sessionId) => {
          void workflow.transitions.openSession(sessionId);
        },
        onFinalizeRecovery: (sessionId) => {
          void homeActions.finalizeRecovery(sessionId);
        },
        onDiscardRecovery: (sessionId) => {
          void homeActions.discardRecovery(sessionId);
        },
        onFilterChange: feed.applyMeetingFilter,
        onNextPage: feed.nextMeetingPage,
        onPreviousPage: feed.previousMeetingPage,
        onExportMeeting: (sessionId, format) => {
          void homeActions.exportMeeting(sessionId, format);
        },
        onExportLedger: (sessionId) => {
          void homeActions.exportMeetingLedger(sessionId);
        },
        onDeleteMeeting: (sessionId) => {
          void homeActions.deleteMeetingFromList(sessionId);
        },
        onRetry: () => {
          void feed.refreshHome();
        },
      },
    };
  }

  if (snapshot === null || snapshot.session.session_id !== screen.sessionId) {
    return {
      screen: "loading",
      model: { kind: "loading", label: t("meetings.loading") },
      actions: null,
    };
  }

  if (
    screen.kind === "gate" ||
    isPreflightMeetingPhase(snapshot.session.phase)
  ) {
    return {
      screen: "gate",
      model: {
        kind: "gate",
        snapshot,
        options:
          screen.kind === "gate"
            ? screen.options
            : setup.startOptions("manual", null, snapshot.session.title),
        refreshing: pendingAction === "preflight_refresh",
        starting: pendingAction === "start",
      },
      actions: {
        onRefresh: () => {
          void startFlow.refreshGate();
        },
        onCancel: () => {
          void startFlow.cancelGate();
        },
        onStart: (consent) => {
          void startFlow.startFromGate(consent);
        },
      },
    };
  }

  if (isActiveMeetingPhase(snapshot.session.phase)) {
    return {
      screen: "live",
      model: { snapshot, pendingAction },
      actions: {
        onPause: () => {
          void actionBindings.pause(snapshot);
        },
        onResume: () => {
          void actionBindings.resume(snapshot);
        },
        onStop: () => {
          void actionBindings.stop(snapshot);
        },
        onDiscard: () => {
          void mutations.discardMeeting(snapshot);
        },
        onCreateNote: (body) => {
          void actionBindings.createNote(
            snapshot,
            body,
            snapshot.session.elapsed_offset_ns,
          );
        },
      },
    };
  }

  return {
    screen: "review",
    model: {
      snapshot,
      lastReceipt: outcomes.lastReceipt,
      pendingAction,
    },
    actions: {
      onBack: workflow.transitions.showHome,
      onTitleSet: (title) => {
        void actionBindings.setTitle(snapshot, title);
      },
      onSpeakerRename: (speakerId, displayName) => {
        void actionBindings.renameSpeaker(snapshot, speakerId, displayName);
      },
      onSpeakerMerge: (sourceSpeakerId, targetSpeakerId) => {
        void actionBindings.mergeSpeakers(
          snapshot,
          sourceSpeakerId,
          targetSpeakerId,
        );
      },
      onSegmentEdit: (segmentId, replacementText, removed) => {
        void actionBindings.editSegment(
          snapshot,
          segmentId,
          replacementText,
          removed,
        );
      },
      onNoteCreate: (body) => {
        void actionBindings.createNote(snapshot, body, null);
      },
      onNoteUpdate: (note, body) => {
        void actionBindings.updateNote(snapshot, note, body);
      },
      onNoteDelete: (note) => {
        void actionBindings.deleteNote(snapshot, note);
      },
      onRegenerate: () => {
        void actionBindings.regenerateArtifacts(snapshot);
      },
      onExport: (format) => {
        void mutations.exportMeeting(snapshot, format);
      },
      onRemoteCancel: () => {
        void mutations.cancelRemote(snapshot);
      },
      onDelete: () => {
        void mutations.deleteMeeting(snapshot);
      },
      onRefresh: () =>
        workflow.transitions.refreshSessionAndHome(snapshot.session.session_id),
    },
  };
};
