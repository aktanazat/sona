import { describe, expect, test } from "bun:test";
import type { EffectiveTranscriptSegment } from "@/bindings";
import { transcriptTurns } from "./TranscriptTab";

/* Turning a list of sentences into the paragraphs a person reads.
 *
 * Two things can go wrong, and both of them lie to the reader. Gluing two
 * speakers together puts words in somebody's mouth. Gluing across a sentence
 * the filter hid joins two remarks made minutes apart under one timestamp, so
 * the run has to break wherever a sentence was left out — which is why the
 * filter runs inside the grouping rather than before it. */

const segment = (
  segmentId: string,
  speakerId: string,
  text: string,
): EffectiveTranscriptSegment => ({
  base: {
    segment_id: segmentId,
    transcript_revision_id: "revision-1",
    track_id: "track-mic",
    ordinal: Number(segmentId.replace(/\D/g, "")),
    start_offset_ns: Number(segmentId.replace(/\D/g, "")) * 1_000_000_000,
    end_offset_ns: (Number(segmentId.replace(/\D/g, "")) + 1) * 1_000_000_000,
    speaker_id: speakerId,
    text,
    confidence_milli: 900,
  },
  replacement_text: null,
  removed: false,
  edit_revision: null,
  assigned_speaker_id: speakerId,
  speaker_assignment: "local_speaker",
});

const TRANSCRIPT = [
  segment("segment-1", "speaker-1", "Okay, ten dollars."),
  segment("segment-2", "speaker-1", "Mm-"),
  segment("segment-3", "speaker-1", "I'm not sure if I can do it."),
  segment("segment-4", "speaker-2", "Yeah."),
  segment("segment-5", "speaker-1", "But"),
  segment("segment-6", "speaker-1", "Okay."),
];

const paragraphs = (segments: readonly EffectiveTranscriptSegment[]) =>
  transcriptTurns(segments, () => true).map((turn) => ({
    speakerId: turn.speakerId,
    texts: turn.segments.map((held) => held.base.text),
  }));

describe("grouping a transcript into turns", () => {
  test("neighbouring sentences by one voice become one turn", () => {
    expect(paragraphs(TRANSCRIPT)).toEqual([
      {
        speakerId: "speaker-1",
        texts: ["Okay, ten dollars.", "Mm-", "I'm not sure if I can do it."],
      },
      { speakerId: "speaker-2", texts: ["Yeah."] },
      { speakerId: "speaker-1", texts: ["But", "Okay."] },
    ]);
  });
  test("a sentence the filter hid breaks the run it sat in", () => {
    const kept = transcriptTurns(
      TRANSCRIPT,
      (held) => held.base.segment_id !== "segment-2",
    ).map((turn) => turn.segments.map((held) => held.base.segment_id));

    expect(kept).toEqual([
      ["segment-1"],
      ["segment-3"],
      ["segment-4"],
      ["segment-5", "segment-6"],
    ]);
  });

  test("no transcript is no turns", () => {
    expect(transcriptTurns([], () => true)).toEqual([]);
  });
});
