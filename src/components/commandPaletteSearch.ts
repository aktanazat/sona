import { defaultFilter } from "cmdk";
import { commands, type QueryRow } from "@/bindings";
import { shouldPackChatTurn } from "./chat/chatModel";

/* ⌘K's half of the one query plane.
 *
 * Everything here is the palette's rules without the palette: what is worth a
 * round trip, how rows become sections, which rows the list may never filter
 * out, and what "ask" actually does. The surface renders these; it decides
 * none of them, which is what makes the whole flow provable without a DOM. */

/** A single letter matches half the corpus, so it is not a query yet. */
export const SEARCH_MIN_CHARS = 2;

/** Long enough to swallow a typed word, short enough to feel like the list is
 * keeping up. The plane's own reads are local, so this is the only latency the
 * reader experiences. */
export const SEARCH_DEBOUNCE_MS = 150;

/** One page. The plane orders by recency, not by score (see `query/mod.rs`), so
 * this is "the newest dozen that matched" — a longer page would not be more
 * relevant, only longer. */
export const SEARCH_LIMIT = 12;

/**
 * The kinds `sona_query_search` actually produces.
 *
 * `QueryRowKind` also declares `series` and `receipt`; no scope emits them, so
 * this list is four, and the declaration order below is the section order the
 * palette renders.
 */
export const RESULT_KINDS = ["meeting", "person", "dictation", "loop"] as const;

export type PaletteResultKind = (typeof RESULT_KINDS)[number];

/** The translation key each section's heading answers to. */
export const resultHeadingKeys = {
  meeting: "commandPalette.search.meetings",
  person: "commandPalette.search.people",
  dictation: "commandPalette.search.dictations",
  loop: "commandPalette.search.loops",
} as const satisfies Record<PaletteResultKind, string>;

export interface PaletteResultSection {
  kind: PaletteResultKind;
  rows: QueryRow[];
}

/**
 * One page of rows as sections, in the fixed kind order, dropping the kinds
 * nothing matched.
 *
 * Inside a section the page order survives untouched: the plane returns newest
 * first, and re-sorting here would be this surface inventing a second answer to
 * "which of these is most relevant" — the one thing the plane deliberately
 * refuses to guess.
 */
export const groupQueryRows = (
  rows: readonly QueryRow[],
): PaletteResultSection[] =>
  RESULT_KINDS.map((kind) => ({
    kind,
    rows: rows.filter((row) => row.kind === kind),
  })).filter((section) => section.rows.length > 0);

/**
 * The three standing facts the ask row is offered on.
 *
 * `enabled` and `paired` are the panel's own (`agent_panel_enabled`,
 * `agent_panel_paired`); `remoteIntelligence` is D14's
 * `meeting_remote_intelligence_enabled`.
 */
export interface AskGate {
  enabled: boolean;
  paired: boolean;
  remoteIntelligence: boolean;
}

/**
 * Whether the ask row is offered.
 *
 * Three settings, not the relay's reachability: an offline relay still takes
 * the question (the panel queues the draft), while an unpaired one has nowhere
 * to send it and the row would be a promise the app cannot keep.
 *
 * `remoteIntelligence` is here because of what [`askSona`] actually sends. The
 * pack is verbatim corpus text — transcript segments and typed notes, quoted
 * straight out of `meeting_search_documents` — and it leaves this Mac for the
 * operator's server. That is the one thing
 * `settings.meetings.remoteIntelligence.consent` promises is off until they
 * turn it on, so the row cannot be offered while it is off, any more than a
 * meeting's summary can be written there. The same consent, and the same
 * per-series exclusion behind it, decides the engine for every other path that
 * ships meeting evidence off-machine (`processing::choose_text_engine`); this
 * is that boundary's half of ⌘K, and `query::pack` is the other.
 */
export const canAsk = (query: string, panel: AskGate): boolean =>
  panel.enabled &&
  panel.paired &&
  panel.remoteIntelligence &&
  query.trim() !== "";

/* cmdk scores every row against what was typed and hides anything that scores
 * zero. That is right for a fixed list of commands and wrong for a corpus: a
 * meeting comes back because the plane's index matched it, sometimes
 * semantically, and its title need not contain a single letter of the query.
 * These two prefixes mark the rows whose membership was already decided
 * elsewhere, and `paletteFilter` gives them the top score so the list may
 * reorder them but never drop them. */
const ROW_VALUE_PREFIX = "sona-row:";
export const ASK_VALUE = "sona-ask";

/** cmdk needs one stable, unique value per row; a `sona://` address is both. */
export const rowValue = (row: QueryRow): string =>
  `${ROW_VALUE_PREFIX}${row.link}`;

/**
 * The palette's scoring rule.
 *
 * Plane rows and the ask row score 1, the ceiling cmdk's own scorer can reach,
 * so nothing the corpus returned can be filtered away and nothing can outrank
 * the ask row's section. Actions keep cmdk's fuzzy match unchanged, which is
 * what keeps "import audio" a one-row list.
 */
export const paletteFilter = (
  value: string,
  search: string,
  keywords?: string[],
): number =>
  value.startsWith(ROW_VALUE_PREFIX) || value === ASK_VALUE
    ? 1
    : defaultFilter(value, search, keywords);

export type PaletteSearchOutcome =
  | { status: "rows"; rows: QueryRow[] }
  | { status: "failed" };

/**
 * One search against the plane.
 *
 * Every failure collapses to one outcome on purpose: the plane's error enum
 * separates a locked corpus from a bad cursor, and neither is something a
 * reader of a search box can act on differently.
 */
export const searchCorpus = async (
  query: string,
): Promise<PaletteSearchOutcome> => {
  try {
    const page = await commands.sonaQuerySearch(
      "all",
      query,
      SEARCH_LIMIT,
      null,
    );
    if (page.status === "error") return { status: "failed" };
    return { status: "rows", rows: page.data.entries };
  } catch {
    return { status: "failed" };
  }
};

/**
 * Open the address a chosen row carries.
 *
 * Through the backend, which is the point: `deeplink.rs` owns what a `sona://`
 * address means and `dispatch_deep_link` owns which surface it wakes — meetings
 * and loops on the meeting navigation event, people, dictations and search on
 * the query-link event. A client-side reading of the same addresses would be a
 * second navigation that agrees with the first until it does not, and a loop's
 * id would have to be taken apart here to find the meeting it belongs to.
 */
export const openRow = (row: QueryRow): Promise<boolean> =>
  commands.sonaOpenLink(row.link);

export type AskOutcome = "sent" | "failed" | "refused";

/**
 * Ask the agent one question about the corpus.
 *
 * The consent is checked here and not only where the row is drawn. Hiding the
 * row is what a reader sees; this is what makes the send impossible. The two
 * would otherwise be one render apart — the palette can be summoned with a
 * question already in it (`sona://search?q=…`), the settings it reads arrive
 * asynchronously, and the only thing standing between an off switch and a
 * transcript on somebody's server would be a boolean computed during paint.
 * `refused` rather than `failed` because a refusal has a reason the reader can
 * act on, and "couldn't send that" would send them looking for a network
 * problem that does not exist.
 *
 * The pack is built next and the turn is refused without it: a question that
 * reached the model with no evidence would be answered from the model's own
 * priors and cited to nothing, which is the exact failure "ask your history" is
 * supposed to end. `sona_query_pack` rejects an empty question, so the caller's
 * gate and the backend's agree.
 *
 * Sending and showing are separate: the turn goes to the backend, which emits
 * the change the chat sheet is already listening for. Whoever calls this owns
 * whether the sheet is open, because the sheet is a fold in the shell's layout
 * and not something a search helper is allowed to reach into.
 */
export const askSona = async (
  question: string,
  locale: string,
  panel: AskGate,
): Promise<AskOutcome> => {
  if (!canAsk(question, panel)) return "refused";
  try {
    const pack = await commands.sonaQueryPack(question);
    if (pack.status === "error") return "failed";
    const sent = await commands.agentPanelSendTurn({
      turn_id: crypto.randomUUID(),
      message: question,
      locale,
      workspace: "sona_chat",
      context_pack: pack.data.pack,
      /* The grant is the sheet's own predicate on the same gate, not a
       * literal: the tools read the corpus the pack was built from, and
       * `canAsk` above has already refused a gate that is off. The turn
       * lands in the sheet, where each lookup shows as a step. */
      tools_allowed: shouldPackChatTurn("sona_chat", panel),
    });
    return sent.status === "error" ? "failed" : "sent";
  } catch {
    return "failed";
  }
};
