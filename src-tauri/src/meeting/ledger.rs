//! The meeting ledger: every thread in a conversation, where it landed, and
//! the transcript quote that says so.
//!
//! Adapted from the `where-did-we-land` skill by gnurio, MIT licence,
//! <https://github.com/gnurio/where-did-we-land>. Taken from it: the
//! nine-state thread vocabulary, the receipt discipline (a state without a
//! verbatim quote does not ship), the four registers, the
//! word-counts-not-words privacy design, and the single self-contained HTML
//! page. See NOTICE for the licence text pointer.
//!
//! Two kinds of claim live in a ledger, and they are deliberately stored in
//! different places:
//!
//! * **Inferred states** — threads, open loops, commitments, stances. A model
//!   reads these out of the transcript, so each one carries a receipt, and
//!   they are cached inside the artifact revision that produced them.
//! * **Measured counts** — turns, seconds, word counts, talk share. These come
//!   from [`super::analytics`], which owns them, and are joined onto the ledger
//!   only when a page is rendered. Storing a copy here would give the same
//!   number two homes: a transcript edit moves the counts without making the
//!   model's reading of the conversation stale.
//!
//! The privacy invariant is upstream's and it is enforced by the wire type: a
//! rendered turn is `[speakerIndex, seconds, wordCount]`, three integers, so
//! there is no field a transcript word could be written into. The only
//! verbatim speech that reaches a page is a receipt quote.

use super::analytics::{MeetingTalkMetrics, Turn};
use super::types::{ArtifactCitation, SpeakerId, TranscriptSegmentId};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;

const TEMPLATE: &str = include_str!("../../resources/ledger/ledger-template.html");

/// The block upstream's page reads its data out of, and the block
/// `check_ledger.py` looks for. Kept byte-identical so an upstream checker
/// still reads a Sona page.
const JSON_OPEN: &str = "<script type=\"application/json\" id=\"convo\">";
const JSON_CLOSE: &str = "</script>";

/// How many threads the first 22 characters of a label are matched on when
/// checking that an unresolved thread reached the open-loops table. Upstream's
/// number, kept so the two checkers agree.
const LABEL_MATCH_CHARS: usize = 22;

/// Where a thread ended up. Upstream's nine-state vocabulary, unchanged: the
/// states a reader can check a receipt against are the states a model is
/// allowed to pick from.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LedgerThreadState {
    /// A choice was made and said out loud.
    Decided,
    /// One party's position was taken up by the other.
    Agreed,
    /// A named person owns a next step.
    Action,
    /// A social or admin thread that ran its course.
    Closed,
    /// Live, and explicitly unresolved.
    Open,
    /// Direction set, specifics missing.
    Partial,
    /// Addressed sideways; the question itself never got answered.
    Ambiguous,
    /// Raised out loud, no response.
    Unanswered,
    /// Died mid-thread on a topic switch.
    Dropped,
}

/// The three-way rollup the state chips and the stat band read. Nine states
/// are what a reader checks against a quote; three are what a glance needs.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LedgerOutcome {
    Landed,
    Open,
    Dropped,
}

impl LedgerThreadState {
    pub const fn outcome(self) -> LedgerOutcome {
        match self {
            Self::Decided | Self::Agreed | Self::Action | Self::Closed => LedgerOutcome::Landed,
            Self::Open | Self::Partial | Self::Ambiguous => LedgerOutcome::Open,
            Self::Unanswered | Self::Dropped => LedgerOutcome::Dropped,
        }
    }

    /// Whether a thread in this state belongs in the open-loops table.
    /// Upstream's `UNRESOLVED` set.
    pub const fn unresolved(self) -> bool {
        matches!(self, Self::Unanswered | Self::Dropped | Self::Ambiguous)
    }

    const fn wire(self) -> &'static str {
        match self {
            Self::Decided => "decided",
            Self::Agreed => "agreed",
            Self::Action => "action",
            Self::Closed => "closed",
            Self::Open => "open",
            Self::Partial => "partial",
            Self::Ambiguous => "ambiguous",
            Self::Unanswered => "unanswered",
            Self::Dropped => "dropped",
        }
    }
}

/// How firmly a commitment was made, read off the language used: "I'll do X"
/// is firm, "we should probably" is not.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LedgerFirmness {
    Firm,
    Soft,
}

impl LedgerFirmness {
    const fn wire(self) -> &'static str {
        match self {
            Self::Firm => "Firm",
            Self::Soft => "Soft",
        }
    }
}

/// The quote a state was read from. `quote` is checked against the transcript
/// mechanically; `t_ms` and `citations` are derived from the cited segment, so
/// a model cannot move a receipt in time.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct LedgerReceipt {
    pub quote: String,
    pub speaker: Option<String>,
    pub t_ms: u64,
    pub citations: Vec<ArtifactCitation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct LedgerThread {
    pub topic: String,
    pub state: LedgerThreadState,
    /// Small talk, agenda-setting and sign-off stay on the timeline and drop
    /// out of the landed score.
    pub substantive: bool,
    pub receipt: LedgerReceipt,
    pub owner: Option<String>,
}

/// A question that was asked out loud, and what happened instead of an answer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct LedgerOpenLoop {
    pub question: String,
    pub instead: String,
    pub at_ms: u64,
    pub citations: Vec<ArtifactCitation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct LedgerCommitment {
    pub who: String,
    pub what: String,
    pub firmness: LedgerFirmness,
    pub receipt: LedgerReceipt,
}

/// A position someone took or changed, and the counterpart it was taken up
/// from when the evidence names one.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct LedgerStance {
    pub from: String,
    pub to: Option<String>,
    pub what: String,
    pub note: Option<String>,
    pub at_ms: u64,
    pub citations: Vec<ArtifactCitation>,
}

/// Whether every receipt in this ledger was found in the transcript. A ledger
/// only reaches `Degraded` after a regeneration also failed the check, and it
/// then carries the counts of what was removed rather than a softer word for
/// it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LedgerReceiptState {
    Verified,
    Degraded {
        dropped_threads: u32,
        dropped_commitments: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
pub struct MeetingLedger {
    /// What someone who read every row knows and the score cannot show.
    pub headline: String,
    pub threads: Vec<LedgerThread>,
    pub open_loops: Vec<LedgerOpenLoop>,
    pub commitments: Vec<LedgerCommitment>,
    pub stances: Vec<LedgerStance>,
    /// What would make a reader wrong to trust this ledger.
    pub caveats: Vec<String>,
    pub receipts: LedgerReceiptState,
}

impl MeetingLedger {
    /// Threads that count towards the landed score.
    pub fn substantive_threads(&self) -> impl Iterator<Item = &LedgerThread> {
        self.threads.iter().filter(|thread| thread.substantive)
    }

    pub fn landed_count(&self) -> usize {
        self.substantive_threads()
            .filter(|thread| thread.state.outcome() == LedgerOutcome::Landed)
            .count()
    }
}

// ── verbatim receipts ───────────────────────────────────────────────────────
//
// The one assertion that catches an invented quote, ported from upstream's
// `check_ledger.py::check_quotes_verbatim`. It runs on this side of the model,
// not inside it: a model asked whether it copied a quote correctly will say
// yes.

/// Fold the transcript and a receipt onto common ground: smart quotes, dashes
/// and ellipses to their ASCII spellings, runs of whitespace to one space,
/// case away. Upstream's `_norm`, minus its leading NFKC pass — Sona carries
/// no Unicode normalisation crate, and the pairs below are the substitutions
/// that decide a receipt match in practice.
pub(crate) fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space_pending = false;
    for character in text.chars() {
        if character.is_whitespace() {
            space_pending = !out.is_empty();
            continue;
        }
        if space_pending {
            out.push(' ');
            space_pending = false;
        }
        match character {
            '\u{2018}' | '\u{2019}' => out.push('\''),
            '\u{201C}' | '\u{201D}' => out.push('"'),
            '\u{2013}' | '\u{2014}' => out.push('-'),
            '\u{2026}' => out.push_str("..."),
            _ => out.extend(character.to_lowercase()),
        }
    }
    out
}

/// The transcript a receipt is checked against: the same evidence text the
/// model was shown, folded and joined. Built from the prompt's evidence rather
/// than from the database, so a receipt taken from a truncated segment still
/// matches the text it was actually read from.
pub(crate) fn fold_haystack<'a>(texts: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for text in texts {
        let folded = fold(text);
        if folded.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&folded);
    }
    out
}

/// A receipt may stitch two stretches of speech together with an ellipsis or a
/// spaced dash. Every fragment on either side of the join still has to exist in
/// the source.
fn fragments(folded_quote: &str) -> impl Iterator<Item = &str> {
    folded_quote
        .split("...")
        .flat_map(|part| part.split(" - "))
        .map(str::trim)
        .filter(|fragment| !fragment.is_empty())
}

/// Whether `quote` appears in the transcript, character for character once both
/// sides are folded.
///
/// Diverges from upstream in one direction only: upstream skips fragments of 25
/// characters or fewer, which leaves a short receipt unchecked. Every fragment
/// is checked here, because a short receipt is exactly where an invented quote
/// hides, and a quote that folds to nothing is not evidence of anything.
fn quote_is_verbatim(quote: &str, folded_haystack: &str) -> bool {
    let folded = fold(quote);
    let mut checked = 0_usize;
    for fragment in fragments(&folded) {
        checked += 1;
        if !folded_haystack.contains(fragment) {
            return false;
        }
    }
    checked > 0
}

/// How many receipts in this ledger are not in the transcript. Zero is the only
/// number a ledger ships with; anything else is regenerated once, then degraded.
pub(crate) fn unverified_receipts(ledger: &MeetingLedger, folded_haystack: &str) -> usize {
    let threads = ledger
        .threads
        .iter()
        .filter(|thread| !quote_is_verbatim(&thread.receipt.quote, folded_haystack))
        .count();
    let commitments = ledger
        .commitments
        .iter()
        .filter(|commitment| !quote_is_verbatim(&commitment.receipt.quote, folded_haystack))
        .count();
    threads + commitments
}

/// Remove every claim whose receipt is not in the transcript and say how many
/// went, in the ledger's own caveats. Nothing unverified ships; nothing is
/// quietly dropped either.
pub(crate) fn degrade_unverified(ledger: &mut MeetingLedger, folded_haystack: &str) {
    let threads_before = ledger.threads.len();
    let commitments_before = ledger.commitments.len();
    ledger
        .threads
        .retain(|thread| quote_is_verbatim(&thread.receipt.quote, folded_haystack));
    ledger
        .commitments
        .retain(|commitment| quote_is_verbatim(&commitment.receipt.quote, folded_haystack));
    let dropped_threads = u32::try_from(threads_before - ledger.threads.len()).unwrap_or(u32::MAX);
    let dropped_commitments =
        u32::try_from(commitments_before - ledger.commitments.len()).unwrap_or(u32::MAX);
    if dropped_threads == 0 && dropped_commitments == 0 {
        return;
    }
    ledger.caveats.push(format!(
        "Receipts not found in this transcript, removed after one regeneration — threads {dropped_threads}, commitments {dropped_commitments}."
    ));
    ledger.receipts = LedgerReceiptState::Degraded {
        dropped_threads,
        dropped_commitments,
    };
}

// ── the page ────────────────────────────────────────────────────────────────
//
// Upstream's field names, because the template's reader is upstream's and
// keeping the names identical keeps the divergence auditable. The fields the
// eval grades against upstream's rubric are visible to the crate; the rest
// are the page's own.

#[derive(Serialize)]
pub(crate) struct PageMeta {
    title: String,
    kind: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<String>,
    start: u64,
    pub(crate) end: u64,
    headline: String,
    /// Not upstream's: a Sona ledger states on the page whether its own
    /// receipt check passed, because the check runs unattended.
    receipts: &'static str,
}

#[derive(Serialize)]
struct PageParticipant {
    name: String,
}

#[derive(Serialize)]
pub(crate) struct PageTopic {
    pub(crate) label: String,
    /// `[fromSeconds, toSeconds, speakerIndex]`, one entry per cited stretch.
    /// Two entries mean the subject was left and came back.
    pub(crate) segs: Vec<(u64, u64, i32)>,
    state: &'static str,
    outcome: &'static str,
    substantive: bool,
    quote: String,
    who: String,
    ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
}

#[derive(Serialize)]
struct PageOpenLoop {
    at: String,
    question: String,
    instead: String,
}

#[derive(Serialize)]
struct PageCommitment {
    owner: String,
    what: String,
    at: String,
    firmness: &'static str,
    quote: String,
}

#[derive(Serialize)]
struct PageStance {
    at: String,
    from: String,
    to: Option<String>,
    what: String,
    note: String,
}

/// One speaker's measured airtime, copied out of [`MeetingTalkMetrics`]
/// without arithmetic. The page displays these numbers; it does not derive
/// them, so the page and the app can never disagree.
#[derive(Serialize)]
pub(crate) struct PageTalkShare {
    pub(crate) name: String,
    #[serde(rename = "sharePermille")]
    pub(crate) share_permille: u32,
    #[serde(rename = "speakingSeconds")]
    speaking_seconds: u64,
    #[serde(rename = "turnCount")]
    pub(crate) turn_count: u32,
    #[serde(rename = "longestMonologueSeconds")]
    longest_monologue_seconds: u64,
}

#[derive(Serialize)]
pub(crate) struct LedgerPage {
    pub(crate) meta: PageMeta,
    participants: Vec<PageParticipant>,
    /// `[speakerIndex, secondsFromStart, wordCount]`. Three integers: the
    /// privacy invariant is the type, not a runtime check. An unattributable
    /// turn carries speaker index `-1` and is left out of the counts.
    pub(crate) turns: Vec<(i32, u64, u32)>,
    pub(crate) topics: Vec<PageTopic>,
    #[serde(rename = "openLoops")]
    open_loops: Vec<PageOpenLoop>,
    commitments: Vec<PageCommitment>,
    stances: Vec<PageStance>,
    #[serde(rename = "talkShare")]
    pub(crate) talk_share: Vec<PageTalkShare>,
    caveats: Vec<String>,
    /// Measured, and shown in the footer.
    #[serde(rename = "interactionCount")]
    interaction_count: u32,
    #[serde(rename = "medianSwitchGapMs", skip_serializing_if = "Option::is_none")]
    median_switch_gap_ms: Option<u64>,
}

/// Everything measured that a page needs, gathered by the caller so this
/// module computes no metric of its own.
pub(crate) struct LedgerPageInput<'a> {
    pub title: &'a str,
    /// The notes template's human label — upstream's `meta.kind`.
    pub kind: &'a str,
    /// `YYYY-MM-DD`. Upstream reads a date only out of the transcript's own
    /// content; Sona recorded the meeting, so its own clock is that content.
    pub date: Option<String>,
    pub duration_ns: u64,
    pub ledger: &'a MeetingLedger,
    pub talk: &'a MeetingTalkMetrics,
    /// Merged speaking turns, from `analytics::merge_turns`. The same list the
    /// talk metrics were counted from.
    pub turns: &'a [Turn],
    pub speaker_names: &'a HashMap<SpeakerId, String>,
    pub segment_speakers: &'a HashMap<TranscriptSegmentId, SpeakerId>,
}

fn mmss(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

const fn seconds(nanos: u64) -> u64 {
    nanos / 1_000_000_000
}

pub(crate) fn build_page(input: LedgerPageInput<'_>) -> LedgerPage {
    let end = seconds(input.duration_ns);
    // Speaker order is the analytics order — most speech first — so the page
    // and the app's analytics strip colour the same person the same way.
    let order: HashMap<SpeakerId, i32> = input
        .talk
        .speakers
        .iter()
        .enumerate()
        .map(|(index, share)| (share.speaker_id, i32::try_from(index).unwrap_or(i32::MAX)))
        .collect();
    let name_of = |speaker_id: SpeakerId| -> String {
        input
            .speaker_names
            .get(&speaker_id)
            .cloned()
            .unwrap_or_else(|| format!("Speaker {}", &speaker_id.uuid().to_string()[..8]))
    };
    let index_of = |speaker_id: SpeakerId| -> i32 { order.get(&speaker_id).copied().unwrap_or(-1) };

    let participants = input
        .talk
        .speakers
        .iter()
        .map(|share| PageParticipant {
            name: name_of(share.speaker_id),
        })
        .collect();
    let turns = input
        .turns
        .iter()
        .map(|turn| {
            (
                index_of(turn.speaker_id),
                seconds(turn.start_offset_ns).min(end),
                turn.word_count,
            )
        })
        .collect();
    let talk_share = input
        .talk
        .speakers
        .iter()
        .map(|share| PageTalkShare {
            name: name_of(share.speaker_id),
            share_permille: share.share_permille,
            speaking_seconds: seconds(share.speaking_ns),
            turn_count: share.turn_count,
            longest_monologue_seconds: seconds(share.longest_monologue_ns),
        })
        .collect();

    let segs_of = |receipt: &LedgerReceipt| -> Vec<(u64, u64, i32)> {
        let mut segs: Vec<(u64, u64, i32)> = receipt
            .citations
            .iter()
            .map(|citation| {
                let from = seconds(citation.start_offset_ns).min(end.saturating_sub(1));
                let to = seconds(citation.end_offset_ns).clamp(from + 1, end.max(from + 1));
                let speaker = input
                    .segment_speakers
                    .get(&citation.segment_id)
                    .copied()
                    .map_or(0, index_of)
                    .max(0);
                (from, to, speaker)
            })
            .collect();
        segs.sort_unstable();
        segs.dedup();
        segs
    };

    let topics = input
        .ledger
        .threads
        .iter()
        .map(|thread| PageTopic {
            label: thread.topic.clone(),
            segs: segs_of(&thread.receipt),
            state: thread.state.wire(),
            outcome: match thread.state.outcome() {
                LedgerOutcome::Landed => "landed",
                LedgerOutcome::Open => "open",
                LedgerOutcome::Dropped => "dropped",
            },
            substantive: thread.substantive,
            quote: thread.receipt.quote.clone(),
            who: thread.receipt.speaker.clone().unwrap_or_default(),
            ts: mmss(thread.receipt.t_ms / 1_000),
            owner: thread.owner.clone(),
        })
        .collect();
    let open_loops: Vec<PageOpenLoop> = input
        .ledger
        .open_loops
        .iter()
        .map(|loop_| PageOpenLoop {
            at: mmss(loop_.at_ms / 1_000),
            question: loop_.question.clone(),
            instead: loop_.instead.clone(),
        })
        .collect();
    let commitments = input
        .ledger
        .commitments
        .iter()
        .map(|commitment| PageCommitment {
            owner: commitment.who.clone(),
            what: commitment.what.clone(),
            at: mmss(commitment.receipt.t_ms / 1_000),
            firmness: commitment.firmness.wire(),
            quote: commitment.receipt.quote.clone(),
        })
        .collect();
    let stances = input
        .ledger
        .stances
        .iter()
        .map(|stance| PageStance {
            at: mmss(stance.at_ms / 1_000),
            from: stance.from.clone(),
            to: stance.to.clone(),
            what: stance.what.clone(),
            note: stance.note.clone().unwrap_or_default(),
        })
        .collect();

    LedgerPage {
        meta: PageMeta {
            title: input.title.to_string(),
            kind: input.kind.to_string(),
            source: "Sona local capture".to_string(),
            date: input.date,
            start: 0,
            end,
            headline: input.ledger.headline.clone(),
            receipts: match input.ledger.receipts {
                LedgerReceiptState::Verified => "verified",
                LedgerReceiptState::Degraded { .. } => "degraded",
            },
        },
        participants,
        turns,
        topics,
        open_loops,
        commitments,
        stances,
        talk_share,
        caveats: input.ledger.caveats.clone(),
        interaction_count: input.talk.interaction_count,
        median_switch_gap_ms: input.talk.median_switch_gap_ms,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LedgerRenderError {
    Serialize,
    Template,
}

/// Write the page: upstream's template with its `#convo` block replaced by
/// this ledger's JSON, and nothing else changed.
pub(crate) fn render_html(page: &LedgerPage) -> Result<String, LedgerRenderError> {
    let json = serde_json::to_string(page).map_err(|_| LedgerRenderError::Serialize)?;
    // A receipt containing `</script>` would end the data block early. `\/` is
    // a legal JSON escape for `/`, so escaping every `</` keeps the JSON
    // byte-for-byte equivalent and the block intact.
    let json = json.replace("</", "<\\/");
    let open = TEMPLATE
        .find(JSON_OPEN)
        .ok_or(LedgerRenderError::Template)?
        + JSON_OPEN.len();
    let close = TEMPLATE[open..]
        .find(JSON_CLOSE)
        .ok_or(LedgerRenderError::Template)?
        + open;
    let mut page_html = String::with_capacity(TEMPLATE.len() + json.len());
    page_html.push_str(&TEMPLATE[..open]);
    page_html.push('\n');
    page_html.push_str(&json);
    page_html.push('\n');
    page_html.push_str(&TEMPLATE[close..]);
    Ok(page_html)
}

// ── the structural checks ───────────────────────────────────────────────────
//
// Upstream's `scripts/check_ledger.py`, which a person runs on a finished page
// and then fixes what it finds. Nobody runs anything by hand here, so the
// checks run at the acceptance seam, on the page this ledger would render as,
// and what they find is logged and, where a reader can weigh it, written into
// the ledger's own caveats. Nothing here rejects a ledger the receipt
// discipline above has accepted.
//
// Some of upstream's checks have no port because they cannot fail on this
// side: `check_no_transcript` (a rendered turn is three integers by type),
// the state half of `check_receipts` (`LedgerThreadState` is an enum), the
// speaker-index half of `check_geometry` (the page resolves every index
// itself), and `check_meta_date` (the exporter formats the date itself). The
// number-clash half of `check_headline_news` is not ported: the prompt
// forbids the repeat, and the check needs a number-word parser that would
// be the largest thing in this section for the least likely failure.

/// A turn is a whole speaking turn, not a subtitle cue. Upstream's bounds: a
/// cue-shaped transcript overshoots the ceiling by ten to twenty times, and
/// an under-split one falls through the floor.
const TURNS_PER_MINUTE_MAX: f64 = 8.0;
const TURNS_PER_MINUTE_MIN: f64 = 0.8;

/// One line `check_ledger.py` would have printed with a cross beside it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CheckFailure {
    /// `check_turn_count`: the page's time axis has no length.
    NoDuration,
    /// `check_turn_count`: turns per minute outside the bounds above.
    TurnDensity { turns: usize, seconds: u64 },
    /// `check_receipts` and `check_quotes_verbatim`: a receipt that is empty
    /// or not in the transcript, by the matcher above.
    UnverifiedReceipt { label: String },
    /// `check_open_loops`: threads left unresolved that never reached the
    /// open-loops table.
    UnlistedUnresolved { count: usize },
    /// `check_geometry`: a cited stretch, in seconds, that does not run
    /// forwards inside the meeting.
    Geometry { label: String, from: u64, to: u64 },
    /// `check_headline_news`: the page opens on nothing.
    MissingHeadline,
}

impl CheckFailure {
    /// What a reader is told, for the failures a reader can weigh. The rest
    /// describe a ledger that never reaches one: an invented receipt is
    /// removed at the seam and an empty headline is refused at validation.
    pub(crate) fn caveat(&self) -> Option<String> {
        match self {
            Self::TurnDensity { turns, seconds } => {
                let clock = mmss(*seconds);
                Some(if turns_per_minute(*turns, *seconds) > TURNS_PER_MINUTE_MAX {
                    format!("Turn count looks cue-shaped: {turns} turns in {clock} is more than a conversation has, so turn counts and talk share are counted over fragments rather than turns.")
                } else {
                    format!("Turn count looks under-split: {turns} turns in {clock} is fewer than a conversation has, so turn counts and talk share are counted over runs longer than one turn.")
                })
            }
            Self::UnlistedUnresolved { count } => Some(format!(
                "Unresolved threads absent from the open-loops table: {count}. Their state was read from a receipt; the question they left open was not written down."
            )),
            Self::NoDuration
            | Self::UnverifiedReceipt { .. }
            | Self::Geometry { .. }
            | Self::MissingHeadline => None,
        }
    }
}

/// Both counts fit a `u32` for any meeting that was recorded, and a `u32` is
/// exact in an `f64`; one that does not is pinned at the top.
fn turns_per_minute(turns: usize, seconds: u64) -> f64 {
    let turns = f64::from(u32::try_from(turns).unwrap_or(u32::MAX));
    let seconds = f64::from(u32::try_from(seconds).unwrap_or(u32::MAX));
    turns / (seconds / 60.0)
}

/// Upstream's `check_open_loops`: every thread left unanswered, dropped or
/// answered sideways is supposed to reach the open-loops table. A thread is
/// listed when the table names it, by the head of its label or by the moment
/// it is cited at, so a question phrased differently from its thread still
/// counts. Counted rather than refused, because the alternative is inventing
/// the question nobody wrote down.
fn unlisted_unresolved(ledger: &MeetingLedger) -> usize {
    let listed = ledger
        .open_loops
        .iter()
        .map(|loop_| {
            format!(
                "{} {} {}",
                mmss(loop_.at_ms / 1_000),
                fold(&loop_.question),
                fold(&loop_.instead)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    ledger
        .threads
        .iter()
        .filter(|thread| thread.state.unresolved())
        .filter(|thread| {
            let topic = fold(&thread.topic);
            let head: String = topic.chars().take(LABEL_MATCH_CHARS).collect();
            !head.is_empty()
                && !listed.contains(&head)
                && !listed.contains(&mmss(thread.receipt.t_ms / 1_000))
        })
        .count()
}

/// Every check upstream runs on a finished page, run on the page this ledger
/// would render as. `folded_haystack` is the transcript as [`fold_haystack`]
/// prepares it: the same text the receipts were accepted against.
pub(crate) fn check(
    ledger: &MeetingLedger,
    page: &LedgerPage,
    folded_haystack: &str,
) -> Vec<CheckFailure> {
    let mut failures = Vec::new();
    let end = page.meta.end;
    if end == 0 {
        failures.push(CheckFailure::NoDuration);
    } else {
        let turns = page.turns.len();
        if !(TURNS_PER_MINUTE_MIN..=TURNS_PER_MINUTE_MAX).contains(&turns_per_minute(turns, end)) {
            failures.push(CheckFailure::TurnDensity {
                turns,
                seconds: end,
            });
        }
    }
    let receipts = ledger
        .threads
        .iter()
        .map(|thread| (thread.topic.as_str(), &thread.receipt))
        .chain(
            ledger
                .commitments
                .iter()
                .map(|commitment| (commitment.what.as_str(), &commitment.receipt)),
        );
    for (label, receipt) in receipts {
        if !quote_is_verbatim(&receipt.quote, folded_haystack) {
            failures.push(CheckFailure::UnverifiedReceipt {
                label: label.to_string(),
            });
        }
        // The page clamps a cited stretch onto its axis rather than refusing
        // it, so the citation is what is checked: a stretch the page had to
        // move is drawn at a time it did not happen.
        for citation in &receipt.citations {
            let to = seconds(citation.end_offset_ns);
            if citation.start_offset_ns >= citation.end_offset_ns || to > end {
                failures.push(CheckFailure::Geometry {
                    label: label.to_string(),
                    from: seconds(citation.start_offset_ns),
                    to,
                });
            }
        }
    }
    let unlisted = unlisted_unresolved(ledger);
    if unlisted > 0 {
        failures.push(CheckFailure::UnlistedUnresolved { count: unlisted });
    }
    if ledger.headline.trim().is_empty() {
        failures.push(CheckFailure::MissingHeadline);
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::analytics::{talk_metrics, AnalyticsSegment};

    const TRANSCRIPT: &str = "I've got a bunch of I think low-level updates for the first the tasks improvement stuff. We never actually said which tier the trial converts into. I'll draft the tier comparison by Friday.";

    fn citation(segment_id: TranscriptSegmentId, start_ns: u64, end_ns: u64) -> ArtifactCitation {
        ArtifactCitation {
            segment_id,
            start_offset_ns: start_ns,
            end_offset_ns: end_ns,
        }
    }

    fn receipt(quote: &str, segment_id: TranscriptSegmentId) -> LedgerReceipt {
        LedgerReceipt {
            quote: quote.to_string(),
            speaker: Some("Dana".to_string()),
            t_ms: 252_000,
            citations: vec![citation(segment_id, 252_000_000_000, 260_000_000_000)],
        }
    }

    fn ledger(threads: Vec<LedgerThread>) -> MeetingLedger {
        MeetingLedger {
            headline: "Pricing came back at the end and is open again.".to_string(),
            threads,
            open_loops: Vec::new(),
            commitments: Vec::new(),
            stances: Vec::new(),
            caveats: Vec::new(),
            receipts: LedgerReceiptState::Verified,
        }
    }

    fn thread(quote: &str, state: LedgerThreadState) -> LedgerThread {
        LedgerThread {
            topic: "Pricing tiers".to_string(),
            state,
            substantive: true,
            receipt: receipt(quote, TranscriptSegmentId::new()),
            owner: None,
        }
    }

    #[test]
    fn a_receipt_copied_from_the_transcript_verifies_through_smart_punctuation() {
        let haystack = fold_haystack([TRANSCRIPT].into_iter());
        // Curly apostrophe, an em dash for a spaced hyphen, and a run of
        // whitespace: none of them change what was said.
        assert!(quote_is_verbatim(
            "I\u{2019}ve got a bunch of I think low\u{2014}level updates",
            &haystack
        ));
        assert!(quote_is_verbatim(
            "We never actually said which tier   the trial converts into.",
            &haystack
        ));
    }

    #[test]
    fn a_tidied_quote_is_not_verbatim() {
        let haystack = fold_haystack([TRANSCRIPT].into_iter());
        // The disfluency was smoothed away. This is the failure the check
        // exists for: it reads well and misrepresents what was said.
        assert!(!quote_is_verbatim(
            "I've got a bunch of low-level updates for the task improvement stuff",
            &haystack
        ));
        assert!(!quote_is_verbatim("", &haystack));
    }

    #[test]
    fn an_elided_receipt_needs_both_fragments_in_the_source() {
        let haystack = fold_haystack([TRANSCRIPT].into_iter());
        assert!(quote_is_verbatim(
            "I've got a bunch of I think low-level updates\u{2026}I'll draft the tier comparison by Friday.",
            &haystack
        ));
        assert!(!quote_is_verbatim(
            "I've got a bunch of I think low-level updates\u{2026}I'll draft the pricing deck by Friday.",
            &haystack
        ));
    }

    #[test]
    fn degrading_removes_the_invented_claim_and_counts_it() {
        let haystack = fold_haystack([TRANSCRIPT].into_iter());
        let mut subject = ledger(vec![
            thread(
                "We never actually said which tier the trial converts into.",
                LedgerThreadState::Open,
            ),
            thread("We agreed to ship annual first.", LedgerThreadState::Agreed),
        ]);
        subject.commitments.push(LedgerCommitment {
            who: "Amir".to_string(),
            what: "Draft the tier comparison".to_string(),
            firmness: LedgerFirmness::Firm,
            receipt: receipt(
                "I'll draft the tier comparison by Friday.",
                TranscriptSegmentId::new(),
            ),
        });

        assert_eq!(unverified_receipts(&subject, &haystack), 1);
        degrade_unverified(&mut subject, &haystack);

        assert_eq!(subject.threads.len(), 1);
        assert_eq!(subject.commitments.len(), 1);
        assert_eq!(
            subject.receipts,
            LedgerReceiptState::Degraded {
                dropped_threads: 1,
                dropped_commitments: 0,
            }
        );
        assert_eq!(subject.caveats.len(), 1);
        assert!(subject.caveats[0].contains("threads 1"));
        assert_eq!(unverified_receipts(&subject, &haystack), 0);
    }

    fn measured() -> (
        MeetingTalkMetrics,
        Vec<Turn>,
        HashMap<SpeakerId, String>,
        HashMap<TranscriptSegmentId, SpeakerId>,
    ) {
        let dana = SpeakerId::new();
        let amir = SpeakerId::new();
        let segment_id = TranscriptSegmentId::new();
        let segments = vec![
            AnalyticsSegment {
                segment_id,
                speaker_id: dana,
                start_offset_ns: 0,
                end_offset_ns: 20_000_000_000,
                text: "We never actually said which tier the trial converts into.".to_string(),
            },
            AnalyticsSegment {
                segment_id: TranscriptSegmentId::new(),
                speaker_id: amir,
                start_offset_ns: 30_000_000_000,
                end_offset_ns: 40_000_000_000,
                text: "I'll draft the tier comparison by Friday.".to_string(),
            },
        ];
        let talk = talk_metrics(&segments);
        let turns = crate::meeting::analytics::merge_turns(&segments);
        let names = HashMap::from([
            (dana, "Dana Whitfield".to_string()),
            (amir, "Amir Haddad".to_string()),
        ]);
        let segment_speakers = segments
            .iter()
            .map(|segment| (segment.segment_id, segment.speaker_id))
            .collect();
        (talk, turns, names, segment_speakers)
    }

    #[test]
    fn airtime_reaches_the_page_as_the_metrics_measured_it() {
        let (talk, turns, names, segment_speakers) = measured();
        let subject = ledger(vec![thread(
            "We never actually said which tier the trial converts into.",
            LedgerThreadState::Open,
        )]);
        let page = build_page(LedgerPageInput {
            title: "Weekly sync",
            kind: "General",
            date: Some("2026-08-29".to_string()),
            duration_ns: 60_000_000_000,
            ledger: &subject,
            talk: &talk,
            turns: &turns,
            speaker_names: &names,
            segment_speakers: &segment_speakers,
        });

        // Shares are copied, not recomputed: the page cannot drift from the
        // strip inside the app.
        for (index, share) in talk.speakers.iter().enumerate() {
            assert_eq!(page.talk_share[index].share_permille, share.share_permille);
            assert_eq!(page.talk_share[index].turn_count, share.turn_count);
        }
        // And every metric turn is on the page exactly once.
        assert_eq!(
            page.turns.len(),
            usize::from(u8::try_from(talk.turn_count).unwrap())
        );
        for (index, _) in talk.speakers.iter().enumerate() {
            let speaker_index = i32::try_from(index).unwrap();
            let counted = page
                .turns
                .iter()
                .filter(|turn| turn.0 == speaker_index)
                .count();
            assert_eq!(
                counted,
                usize::from(u8::try_from(talk.speakers[index].turn_count).unwrap())
            );
        }
    }

    #[test]
    fn the_page_carries_counts_and_no_speech_beyond_its_receipts() {
        let (talk, turns, names, segment_speakers) = measured();
        let subject = ledger(vec![thread(
            "We never actually said which tier the trial converts into.",
            LedgerThreadState::Open,
        )]);
        let page = build_page(LedgerPageInput {
            title: "Weekly sync",
            kind: "General",
            date: Some("2026-08-29".to_string()),
            duration_ns: 60_000_000_000,
            ledger: &subject,
            talk: &talk,
            turns: &turns,
            speaker_names: &names,
            segment_speakers: &segment_speakers,
        });
        let html = render_html(&page).expect("rendered page");

        assert!(html.contains(JSON_OPEN));
        // The one receipt is on the page; the other speaker's words are not.
        assert!(html.contains("which tier the trial converts into"));
        assert!(!html.contains("draft the tier comparison"));
        // Word counts, not words.
        assert!(html.contains("[0,0,10]") || html.contains("[0, 0, 10]"));
    }

    #[test]
    fn a_receipt_cannot_close_the_data_block() {
        let (talk, turns, names, segment_speakers) = measured();
        let mut subject = ledger(vec![thread(
            "We never actually said which tier the trial converts into.",
            LedgerThreadState::Open,
        )]);
        subject.headline = "Ends with </script><script>alert(1)</script>".to_string();
        let page = build_page(LedgerPageInput {
            title: "Weekly sync",
            kind: "General",
            date: None,
            duration_ns: 60_000_000_000,
            ledger: &subject,
            talk: &talk,
            turns: &turns,
            speaker_names: &names,
            segment_speakers: &segment_speakers,
        });
        let html = render_html(&page).expect("rendered page");
        let block = html
            .split_once(JSON_OPEN)
            .expect("data block")
            .1
            .split_once(JSON_CLOSE)
            .expect("closing tag")
            .0;
        assert!(!block.contains("</script"));
        assert!(serde_json::from_str::<serde_json::Value>(block.trim()).is_ok());
    }

    #[test]
    fn an_unresolved_thread_is_listed_by_its_label_or_by_its_moment() {
        let mut subject = ledger(vec![thread(
            "We never actually said which tier the trial converts into.",
            LedgerThreadState::Unanswered,
        )]);
        assert_eq!(unlisted_unresolved(&subject), 1);
        // Worded nothing like the thread, and cited somewhere else: not it.
        subject.open_loops.push(LedgerOpenLoop {
            question: "Who owns the audit log?".to_string(),
            instead: "Nobody answered.".to_string(),
            at_ms: 40_000,
            citations: Vec::new(),
        });
        assert_eq!(unlisted_unresolved(&subject), 1);
        // Worded nothing like the thread, but cited at its moment: upstream
        // counts that as the same question, and so does this.
        subject.open_loops.push(LedgerOpenLoop {
            question: "Does the trial land people on team?".to_string(),
            instead: "The discount got answered instead.".to_string(),
            at_ms: 252_000,
            citations: Vec::new(),
        });
        assert_eq!(unlisted_unresolved(&subject), 0);
        // And the label's head on its own is enough.
        subject.open_loops.truncate(1);
        subject.open_loops.push(LedgerOpenLoop {
            question: "Which tier does the trial convert into?".to_string(),
            instead: "Pricing tiers never came back up.".to_string(),
            at_ms: 40_000,
            citations: Vec::new(),
        });
        assert_eq!(unlisted_unresolved(&subject), 0);
    }

    /// A whole meeting, rendered to a real file, and checked the way a reader
    /// checking this page would have to: parse the data block out of it, look
    /// every receipt up in the transcript, and count what speech reached the
    /// page that was not a receipt.
    ///
    /// The pair of files it leaves in the temp directory is the smoke artifact:
    /// upstream's `scripts/check_ledger.py <page> <transcript>` reads both and
    /// exits 0 on them, which is the check this adaptation has to keep passing.
    #[test]
    fn renders_a_standalone_page_whose_only_speech_is_its_receipts() {
        let dana = SpeakerId::new();
        let amir = SpeakerId::new();
        let updates = TranscriptSegmentId::new();
        let tiers = TranscriptSegmentId::new();
        let admin = TranscriptSegmentId::new();
        let comparison = TranscriptSegmentId::new();
        let segments = vec![
            AnalyticsSegment {
                segment_id: updates,
                speaker_id: dana,
                start_offset_ns: 0,
                end_offset_ns: 20_000_000_000,
                text: "I've got a bunch of I think low-level updates for the first the tasks improvement stuff.".to_string(),
            },
            AnalyticsSegment {
                segment_id: tiers,
                speaker_id: amir,
                start_offset_ns: 22_000_000_000,
                end_offset_ns: 95_000_000_000,
                text: "We never actually said which tier the trial converts into.".to_string(),
            },
            // Never quoted, and full of words nothing else on the page uses:
            // if any of them reach the file, the transcript leaked.
            AnalyticsSegment {
                segment_id: admin,
                speaker_id: dana,
                start_offset_ns: 100_000_000_000,
                end_offset_ns: 140_000_000_000,
                text: "The aubergine sticker budget and the quarterly rebate spreadsheet both need owners.".to_string(),
            },
            AnalyticsSegment {
                segment_id: comparison,
                speaker_id: amir,
                start_offset_ns: 150_000_000_000,
                end_offset_ns: 180_000_000_000,
                text: "I'll draft the tier comparison by Friday.".to_string(),
            },
        ];
        let transcript: Vec<&str> = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        let haystack = fold_haystack(transcript.iter().copied());

        let cite = |segment_id, start_ns: u64, end_ns: u64| LedgerReceipt {
            quote: segments
                .iter()
                .find(|segment| segment.segment_id == segment_id)
                .expect("fixture segment")
                .text
                .clone(),
            speaker: None,
            t_ms: start_ns / 1_000_000,
            citations: vec![citation(segment_id, start_ns, end_ns)],
        };
        let subject = MeetingLedger {
            headline: "Pricing came back at the end and nobody closed it.".to_string(),
            threads: vec![
                LedgerThread {
                    topic: "Low-level updates".to_string(),
                    state: LedgerThreadState::Closed,
                    substantive: false,
                    receipt: cite(updates, 0, 20_000_000_000),
                    owner: None,
                },
                LedgerThread {
                    topic: "Pricing tiers".to_string(),
                    state: LedgerThreadState::Unanswered,
                    substantive: true,
                    receipt: cite(tiers, 22_000_000_000, 95_000_000_000),
                    owner: None,
                },
                LedgerThread {
                    topic: "Tier comparison".to_string(),
                    state: LedgerThreadState::Action,
                    substantive: true,
                    receipt: cite(comparison, 150_000_000_000, 180_000_000_000),
                    owner: Some("Amir".to_string()),
                },
            ],
            open_loops: vec![LedgerOpenLoop {
                question: "Which pricing tiers does the trial convert into?".to_string(),
                instead: "The discount question got answered instead.".to_string(),
                at_ms: 22_000,
                citations: vec![citation(tiers, 22_000_000_000, 95_000_000_000)],
            }],
            commitments: vec![LedgerCommitment {
                who: "Amir".to_string(),
                what: "Draft the tier comparison".to_string(),
                firmness: LedgerFirmness::Firm,
                receipt: cite(comparison, 150_000_000_000, 180_000_000_000),
            }],
            stances: vec![LedgerStance {
                from: "Amir".to_string(),
                to: Some("Dana".to_string()),
                what: "Ship the annual plan first".to_string(),
                note: Some("Taken up without pushback.".to_string()),
                at_ms: 150_000,
                citations: vec![citation(comparison, 150_000_000_000, 180_000_000_000)],
            }],
            caveats: vec![
                "Speaker labels came from diarization, not from names anyone said.".to_string(),
            ],
            receipts: LedgerReceiptState::Verified,
        };
        assert_eq!(unverified_receipts(&subject, &haystack), 0);

        let talk = talk_metrics(&segments);
        let turns = crate::meeting::analytics::merge_turns(&segments);
        let names = HashMap::from([
            (dana, "Dana Whitfield".to_string()),
            (amir, "Amir Haddad".to_string()),
        ]);
        let segment_speakers: HashMap<TranscriptSegmentId, SpeakerId> = segments
            .iter()
            .map(|segment| (segment.segment_id, segment.speaker_id))
            .collect();
        let page = build_page(LedgerPageInput {
            title: "Where did we land?",
            kind: "Meeting",
            date: Some("2026-08-29".to_string()),
            duration_ns: 200_000_000_000,
            ledger: &subject,
            talk: &talk,
            turns: &turns,
            speaker_names: &names,
            segment_speakers: &segment_speakers,
        });
        let html = render_html(&page).expect("rendered page");
        // The same page passes the ported checker, so what upstream's script
        // accepts on disk and what `check` accepts in memory stay one thing.
        assert_eq!(check(&subject, &page, &haystack), Vec::new());

        let directory = std::env::temp_dir();
        let page_path = directory.join("sona-ledger-fixture.html");
        let transcript_path = directory.join("sona-ledger-fixture-transcript.txt");
        std::fs::write(&page_path, &html).expect("write page");
        std::fs::write(&transcript_path, transcript.join("\n")).expect("write transcript");

        // 1. The data block is there and it is JSON.
        let block = html
            .split_once(JSON_OPEN)
            .expect("data block")
            .1
            .split_once(JSON_CLOSE)
            .expect("closing tag")
            .0;
        let data: serde_json::Value =
            serde_json::from_str(block.trim()).expect("data block parses");

        // 2. Every turn is three integers. Word counts, never words.
        let rendered_turns = data["turns"].as_array().expect("turns");
        assert_eq!(rendered_turns.len(), turns.len());
        for turn in rendered_turns {
            let triple = turn.as_array().expect("turn triple");
            assert_eq!(triple.len(), 3);
            assert!(triple.iter().all(|value| value.is_i64() || value.is_u64()));
        }

        // 3. Every receipt on the page is in the transcript.
        for topic in data["topics"].as_array().expect("topics") {
            let quote = topic["quote"].as_str().expect("quote");
            assert!(
                quote_is_verbatim(quote, &haystack),
                "receipt not verbatim: {quote}"
            );
        }

        // 4. Nothing else said in this meeting reached the file. A word is
        //    leaked speech when it was spoken, is in no receipt, and is not
        //    already part of the empty template's own prose.
        let words = |text: &str| -> std::collections::BTreeSet<String> {
            fold(text)
                .split(|character: char| !character.is_alphanumeric())
                .filter(|word| word.len() > 2)
                .map(str::to_string)
                .collect()
        };
        let receipt_words = words(
            &subject
                .threads
                .iter()
                .map(|thread| thread.receipt.quote.as_str())
                .chain(
                    subject
                        .commitments
                        .iter()
                        .map(|commitment| commitment.receipt.quote.as_str()),
                )
                .collect::<Vec<_>>()
                .join(" "),
        );
        let template_words = words(TEMPLATE);
        let rendered_words = words(&html);
        let leaked: Vec<String> = words(&transcript.join(" "))
            .into_iter()
            .filter(|word| !receipt_words.contains(word))
            .filter(|word| !template_words.contains(word))
            .filter(|word| rendered_words.contains(word))
            .collect();
        assert!(leaked.is_empty(), "transcript words leaked: {leaked:?}");
        // And the check has teeth: the unquoted segment's words exist to be
        // caught, so a page that embedded the transcript would fail above.
        assert!(words("aubergine quarterly rebate spreadsheet")
            .iter()
            .all(|word| !rendered_words.contains(word)));
    }
}
