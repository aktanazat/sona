import { useCallback } from "react";
import {
  commands,
  type ManualNote,
  type MeetingReviewSnapshot,
  type SpeakerId,
} from "@/bindings";
import type { MeetingMutations } from "./useMeetingMutations";

export interface MeetingActionBindings {
  pause: (snapshot: MeetingReviewSnapshot) => Promise<void>;
  resume: (snapshot: MeetingReviewSnapshot) => Promise<void>;
  stop: (snapshot: MeetingReviewSnapshot) => Promise<void>;
  createNote: (
    snapshot: MeetingReviewSnapshot,
    body: string,
    startOffsetNs: number | null,
  ) => Promise<void>;
  setTitle: (snapshot: MeetingReviewSnapshot, title: string) => Promise<void>;
  renameSpeaker: (
    snapshot: MeetingReviewSnapshot,
    speakerId: SpeakerId,
    displayName: string,
  ) => Promise<void>;
  mergeSpeakers: (
    snapshot: MeetingReviewSnapshot,
    sourceSpeakerId: SpeakerId,
    targetSpeakerId: SpeakerId,
  ) => Promise<void>;
  editSegment: (
    snapshot: MeetingReviewSnapshot,
    segmentId: string,
    replacementText: string,
    removed: boolean,
  ) => Promise<void>;
  updateNote: (
    snapshot: MeetingReviewSnapshot,
    note: ManualNote,
    body: string,
  ) => Promise<void>;
  deleteNote: (
    snapshot: MeetingReviewSnapshot,
    note: ManualNote,
  ) => Promise<void>;
  regenerateArtifacts: (snapshot: MeetingReviewSnapshot) => Promise<void>;
}

/** Binds each named UI action to its generated command. The mutation owner
 * still runs every command and owns the write lifecycle. */
export const useMeetingActionBindings = (
  mutations: MeetingMutations,
): MeetingActionBindings => {
  const pause = useCallback(
    (snapshot: MeetingReviewSnapshot) =>
      mutations.mutateSession("pause", snapshot, (operationId) =>
        commands.meetingPause({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
        }),
      ),
    [mutations],
  );

  const resume = useCallback(
    (snapshot: MeetingReviewSnapshot) =>
      mutations.mutateSession("resume", snapshot, (operationId) =>
        commands.meetingResume({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
        }),
      ),
    [mutations],
  );

  const stop = useCallback(
    (snapshot: MeetingReviewSnapshot) =>
      mutations.mutateSession("stop", snapshot, (operationId) =>
        commands.meetingStop(
          {
            operation_id: operationId,
            session_id: snapshot.session.session_id,
            expected_revision: snapshot.session.revision,
          },
          "meeting_live",
        ),
      ),
    [mutations],
  );

  const createNote = useCallback(
    (
      snapshot: MeetingReviewSnapshot,
      body: string,
      startOffsetNs: number | null,
    ) =>
      mutations.mutateSession("note_create", snapshot, (operationId) =>
        commands.meetingNoteCreate({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
          start_offset_ns: startOffsetNs,
          end_offset_ns: null,
          body,
        }),
      ),
    [mutations],
  );

  const setTitle = useCallback(
    (snapshot: MeetingReviewSnapshot, title: string) =>
      mutations.mutateSession("title_set", snapshot, (operationId) =>
        commands.meetingTitleSet({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
          title,
        }),
      ),
    [mutations],
  );

  const renameSpeaker = useCallback(
    (
      snapshot: MeetingReviewSnapshot,
      speakerId: SpeakerId,
      displayName: string,
    ) =>
      mutations.mutateSession("speaker_rename", snapshot, (operationId) =>
        commands.meetingSpeakerRename({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
          speaker_id: speakerId,
          display_name: displayName,
        }),
      ),
    [mutations],
  );

  const mergeSpeakers = useCallback(
    (
      snapshot: MeetingReviewSnapshot,
      sourceSpeakerId: SpeakerId,
      targetSpeakerId: SpeakerId,
    ) =>
      mutations.mutateSession("speaker_merge", snapshot, (operationId) =>
        commands.meetingSpeakerMerge({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
          source_speaker_id: sourceSpeakerId,
          target_speaker_id: targetSpeakerId,
        }),
      ),
    [mutations],
  );

  const editSegment = useCallback(
    (
      snapshot: MeetingReviewSnapshot,
      segmentId: string,
      replacementText: string,
      removed: boolean,
    ) =>
      mutations.mutateSession("segment_edit", snapshot, (operationId) =>
        commands.meetingSegmentEdit({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
          segment_id: segmentId,
          replacement_text: replacementText,
          removed,
        }),
      ),
    [mutations],
  );

  const updateNote = useCallback(
    (snapshot: MeetingReviewSnapshot, note: ManualNote, body: string) =>
      mutations.mutateSession("note_update", snapshot, (operationId) =>
        commands.meetingNoteUpdate({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
          note_id: note.note_id,
          expected_note_revision: note.revision,
          start_offset_ns: note.start_offset_ns,
          end_offset_ns: note.end_offset_ns,
          body,
        }),
      ),
    [mutations],
  );

  const deleteNote = useCallback(
    (snapshot: MeetingReviewSnapshot, note: ManualNote) =>
      mutations.mutateSession("note_delete", snapshot, (operationId) =>
        commands.meetingNoteDelete({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
          note_id: note.note_id,
          expected_note_revision: note.revision,
        }),
      ),
    [mutations],
  );

  const regenerateArtifacts = useCallback(
    (snapshot: MeetingReviewSnapshot) =>
      mutations.mutateSession("artifacts_regenerate", snapshot, (operationId) =>
        commands.meetingArtifactsRegenerate({
          operation_id: operationId,
          session_id: snapshot.session.session_id,
          expected_revision: snapshot.session.revision,
        }),
      ),
    [mutations],
  );

  return {
    pause,
    resume,
    stop,
    createNote,
    setTitle,
    renameSpeaker,
    mergeSpeakers,
    editSegment,
    updateNote,
    deleteNote,
    regenerateArtifacts,
  };
};
