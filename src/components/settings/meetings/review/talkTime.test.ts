import { describe, expect, test } from "bun:test";
import { talkTimeSlices } from "./talkTime";
import type { SpeakerTalkShare } from "../meetingAnalytics";

const share = (speakerId: string, sharePermille: number): SpeakerTalkShare => ({
  speaker_id: speakerId,
  speaking_ns: sharePermille * 1_000_000,
  share_permille: sharePermille,
  turn_count: 1,
  longest_monologue_ns: 0,
});

const named = (speakerId: string) => `Speaker ${speakerId}`;

describe("talkTimeSlices", () => {
  test("orders speakers by how much of the meeting they held", () => {
    const slices = talkTimeSlices(
      [share("quiet", 200), share("loud", 600), share("middle", 200)],
      named,
    );

    expect(slices.map((slice) => slice.speakerId)).toEqual([
      "loud",
      "quiet",
      "middle",
    ]);
    expect(slices[0].permille).toBe(600);
    expect(slices[0].name).toBe("Speaker loud");
  });
  test("a speaker who said nothing is left off the list", () => {
    const slices = talkTimeSlices(
      [share("heard", 1_000), share("silent", 0)],
      named,
    );

    expect(slices).toHaveLength(1);
    expect(slices[0].speakerId).toBe("heard");
  });

  test("no measured speech produces no speaker shares", () => {
    expect(talkTimeSlices([], named)).toEqual([]);
    expect(talkTimeSlices([share("a", 0), share("b", 0)], named)).toEqual([]);
  });
});
