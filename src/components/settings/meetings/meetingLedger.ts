import type {
  LedgerThreadState,
  MeetingArtifactRevision,
  MeetingLedger,
} from "@/bindings";

/* The ledger's frontend vocabulary. The wire shapes are generated from
 * `src-tauri/src/meeting/ledger.rs`, so what is left here is the part the
 * backend does not hand over: the rollup the chips read, and picking which
 * revision's ledger is the current one.
 *
 * Adapted from the where-did-we-land skill by gnurio (MIT licence,
 * https://github.com/gnurio/where-did-we-land). See NOTICE. */

/** Nine states are what a reader checks against a quote; three are what a
 *  glance needs. Mirrors `LedgerThreadState::outcome` in Rust, which the page
 *  exporter uses — the two must agree on what "landed" means. */
export type LedgerOutcome = "landed" | "open" | "dropped";

export const LEDGER_OUTCOME = {
  decided: "landed",
  agreed: "landed",
  action: "landed",
  closed: "landed",
  open: "open",
  partial: "open",
  ambiguous: "open",
  unanswered: "dropped",
  dropped: "dropped",
} satisfies Record<LedgerThreadState, LedgerOutcome>;

/** The newest current revision's ledger, or null. Revisions come newest
 *  first, and one generated before ledgers existed carries none.
 *
 *  Takes the two fields it reads rather than a whole revision, so the
 *  follow-up agent's narrower message source can ask the review page's
 *  question instead of spelling the same three conditions out again. */
export const currentLedger = (
  artifacts: ReadonlyArray<Pick<MeetingArtifactRevision, "state" | "content">>,
): MeetingLedger | null => {
  for (const artifact of artifacts) {
    if (artifact.state !== "current") continue;
    const ledger = artifact.content?.ledger;
    if (ledger) return ledger;
  }
  return null;
};
