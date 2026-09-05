import React from "react";
import { useCommandState } from "cmdk";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands, type QueryRow, type SavedPrompt } from "@/bindings";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/vg/command";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/vg/dialog";
import { cn } from "@/lib/cn";
import { formatRelativeTime } from "@/lib/utils/format";
import {
  groupPaletteActions,
  type CommandPaletteAction,
} from "./commandPaletteActions";
import {
  askSona,
  ASK_VALUE,
  canAsk,
  groupQueryRows,
  openRow,
  paletteFilter,
  resultHeadingKeys,
  rowValue,
  searchCorpus,
  SEARCH_DEBOUNCE_MS,
  SEARCH_MIN_CHARS,
  type AskGate,
} from "./commandPaletteSearch";
import {
  promptFailureKeys,
  runSavedPrompt,
  usePromptShellStore,
} from "./settings/meetings/promptTargets";

/* The command palette: cmdk inside the shared dialog, and nothing else.
 *
 * It is mounted eagerly with the shell. The previous version lazy-loaded this
 * surface behind `<Suspense fallback={null}>` and latched a second `summoned`
 * flag beside the parent's `open`, which meant the first chord painted nothing
 * at all until the chunk landed and then started an entrance spring from
 * opacity 0 — press, blank, appear. That gap is what made people press the
 * chord again, and the second press toggled it shut. Neither the chunk nor the
 * latch is worth a flicker on the app's primary navigation surface.
 *
 * Motion is gone from this path too: cmdk owns the highlight and the
 * scroll-into-view, Radix owns focus and dismissal, and the only animation is
 * the dialog's own 150ms fade and scale. The global reduced-motion rule in
 * App.css collapses that for anyone who asked.
 *
 * `Dialog` + `Command` rather than the kit's `CommandDialog` wrapper lets this
 * surface apply the shared sentence-case group-label role directly.
 *
 * Typing is now a search of the corpus, not only a filter of this list: two
 * characters in, the query plane answers with meetings, people, dictations and
 * open loops, and Enter on one of them opens its `sona://` address through the
 * same dispatch an external deep link takes. An empty field is exactly the list
 * it always was.
 *
 * Two orderings meet here, so both are named. Inside a section the plane's page
 * order survives untouched (newest first). Between sections cmdk sorts by best
 * score, and `paletteFilter` scores every plane row 1 — the ceiling — so a
 * typed question puts the corpus above the command list, an exactly-matching
 * command ties and stays above it, and the ask row, alone in the last section,
 * can be tied but never beaten. */

export interface CommandPaletteProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  actions: readonly CommandPaletteAction[];
  /**
   * A question the shell was asked to search for — `sona://search?q=…` arriving
   * while the app is running. The nonce is what makes the same question twice a
   * second request rather than a no-op.
   */
  seed: { query: string; nonce: number } | null;
  /**
   * The agent's three standing facts, which is what the ask row is gated on:
   * the toggle in Settings, whether this machine is paired to a relay, and
   * whether the operator has consented to meeting evidence being written on
   * that relay. See `canAsk` for why the third one belongs here.
   */
  panel: AskGate;
  /**
   * Show the chat sheet, because that is where the answer will arrive. The
   * palette sends the turn; it does not own the surface that reads it.
   */
  onAsk: () => void;
}

/* One row of the list, and one heading over a set of them. Each was spelled out
 * four and three times respectively; they are one string each now, because a
 * row that reads differently in the actions section than in the results section
 * is the shape this surface keeps regressing into.
 *
 * No glyph on a row. Every one of them sat beside a word that already said the
 * same thing — a camera beside "Meetings", a folder beside "Open recordings
 * folder" — so a list of ten destinations and verbs was twenty marks to read
 * instead of ten. The words are the list. */
const ROW =
  "min-h-9 gap-2.5 rounded-md px-2 py-2 text-[14px] text-gray-1000 data-[selected=true]:bg-gray-alpha-300";

/* 12px, secondary, sentence case: a heading over rows is the smallest type on
 * the surface, not a second row. It shipped at the rows' own size, which made
 * every section label compete with the things under it. */
const GROUP =
  "p-1.5 [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:pt-2 [&_[cmdk-group-heading]]:pb-1 [&_[cmdk-group-heading]]:text-[12px] [&_[cmdk-group-heading]]:leading-4 [&_[cmdk-group-heading]]:text-gray-900";

interface ResultRowProps {
  row: QueryRow;
  now: number;
  onSelect: () => void;
}

/**
 * One noun from the corpus: what it is, what it is called, the words that
 * matched, and when.
 *
 * Two lines rather than one because a title without its matched text is a
 * search result you have to open to evaluate. The time is the only number on
 * the row, so it sits at the end where the eye can skip it.
 */
const ResultRow: React.FC<ResultRowProps> = ({ row, now, onSelect }) => (
  <CommandItem
    value={rowValue(row)}
    onSelect={onSelect}
    className={cn(ROW, "items-start")}
  >
    <span className="flex min-w-0 flex-1 flex-col gap-0.5">
      <span className="truncate">{row.title}</span>
      {row.snippet !== "" && (
        <span className="truncate text-[12px] text-gray-900">
          {row.snippet}
        </span>
      )}
    </span>
    <span className="flex-none pt-0.5 text-[12px] text-gray-900 tabular-nums">
      {formatRelativeTime(row.when_utc_ms, now)}
    </span>
  </CommandItem>
);

/**
 * The one sentence a failed search is allowed.
 *
 * It renders only while the list has rows of its own: with an empty list the
 * empty state already carries it, and the panel's rule holds here too — a datum
 * appears once per screen.
 */
const SearchNotice: React.FC<{ message: string }> = ({ message }) => {
  const count = useCommandState((state) => state.filtered.count);
  if (count === 0) return null;
  return (
    /* Its own row, divided off the list above it: a sentence tucked under the
     * last group read as that group's own footnote, and this one is about the
     * whole search. gray-900 rather than gray-800 because it is prose — 3.0:1
     * is not a contrast a sentence somebody has to read may ship at. */
    <p
      className="border-t border-gray-alpha-400 px-4 py-2 text-[12px] leading-4 text-gray-900"
      role="status"
    >
      {message}
    </p>
  );
};

/**
 * One search, as this surface reads it.
 *
 * `pending` carries the page it is about to replace, so typing the next letter
 * does not blank the list the reader is looking at — and, more to the point,
 * so nothing claims the corpus came back empty while its answer is still out.
 */
type PaletteSearch =
  /** Nothing worth a round trip is in the field. */
  | { status: "idle" }
  | { status: "pending"; rows: readonly QueryRow[] }
  | { status: "rows"; rows: readonly QueryRow[] }
  | { status: "failed" };

const searchRows = (search: PaletteSearch): readonly QueryRow[] =>
  search.status === "rows" || search.status === "pending" ? search.rows : [];

export const CommandPalette: React.FC<CommandPaletteProps> = ({
  open,
  onOpenChange,
  actions,
  seed,
  panel,
  onAsk,
}) => {
  const { t, i18n } = useTranslation();
  const [query, setQuery] = React.useState("");
  const [search, setSearch] = React.useState<PaletteSearch>({ status: "idle" });
  /* cmdk re-selects the first row on every keystroke, but a page that arrives
   * 150ms later is not a keystroke: without owning the selection, the highlight
   * would stay on whichever command was matched while the corpus answered, and
   * Enter would run it instead of opening the row the reader is looking at. */
  const [selected, setSelected] = React.useState("");
  const [prompts, setPrompts] = React.useState<readonly SavedPrompt[]>([]);
  const requestRef = React.useRef(0);
  /* Read once per render and handed down, so every row on one paint measures
   * "2 minutes ago" from the same instant. A palette is open for seconds; a
   * clock of its own would be a ticking timer nobody reads. */
  const now = Date.now();

  const sections = groupPaletteActions(actions);
  const groupLabels = {
    navigation: t("commandPalette.navigation"),
    actions: t("commandPalette.actions"),
  } satisfies Record<CommandPaletteAction["group"], string>;
  const results = groupQueryRows(searchRows(search));
  /* The one sentence a settled search is allowed, and the reason this state is
   * four-valued rather than a pair of booleans. `CommandEmpty` is cmdk's
   * `filtered.count === 0` branch, and the ask row scores 1 whenever the field
   * has text and the agent is paired — so on a paired install that branch can
   * never fire, and a search that matched nothing used to render nothing at
   * all. A palette that looks identical whether the corpus answered "none",
   * has not answered yet, or cannot be read is a palette people report as
   * broken, which is exactly what happened. */
  const notice =
    search.status === "failed"
      ? t("commandPalette.search.unavailable")
      : search.status === "rows" && search.rows.length === 0
        ? t("commandPalette.search.empty", { query: query.trim() })
        : null;
  const asking = canAsk(query, panel);
  /* Prompts are offered only while a record has said what it is: a prompt is
   * asked *about* something, and the palette is the one surface with no record
   * of its own. See `promptTargets`. */
  const promptTarget = usePromptShellStore((state) => state.target);

  React.useEffect(() => {
    if (seed === null) return;
    setQuery(seed.query);
  }, [seed]);

  /* Closing clears the field. A palette that reopens holding last week's
   * question would also reopen holding last week's answers. */
  React.useEffect(() => {
    if (open) return;
    setQuery("");
    setSearch({ status: "idle" });
  }, [open]);

  /* Read on open rather than once at mount, so a prompt written a minute ago in
   * Settings is offered by the next press of the chord. */
  React.useEffect(() => {
    if (!open || promptTarget === null) return;
    void commands.savedPromptList().then((result) => {
      setPrompts(result.status === "ok" ? result.data.prompts : []);
    });
  }, [open, promptTarget]);

  React.useEffect(() => {
    const question = query.trim();
    const request = requestRef.current + 1;
    requestRef.current = request;
    if (question.length < SEARCH_MIN_CHARS) {
      setSearch({ status: "idle" });
      return;
    }
    setSearch((current) => ({ status: "pending", rows: searchRows(current) }));
    const timer = setTimeout(() => {
      void searchCorpus(question).then((outcome) => {
        // A page that lost its race is a page for a query nobody is reading.
        if (requestRef.current !== request) return;
        setSearch(outcome);
        if (outcome.status === "rows" && outcome.rows.length > 0) {
          setSelected(rowValue(outcome.rows[0]));
        }
      });
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query]);

  const choose = (row: QueryRow) => {
    /* Closed first, then routed, for the same reason an action is: the close
     * rides inside the same frame as whatever the address navigates to. */
    onOpenChange(false);
    void openRow(row);
  };

  const ask = () => {
    const question = query.trim();
    onOpenChange(false);
    /* Opened before the pack is built, not after the turn lands: assembling
     * the evidence is a round trip through the corpus, and a palette that
     * closes onto an unchanged page for half a second reads as a press that
     * did nothing. */
    onAsk();
    void askSona(question, i18n.language, panel).then((outcome) => {
      /* Two refusals, two sentences. A refused ask is the consent gate, and
       * naming a network problem the reader does not have would send them
       * looking in the wrong place for a switch they own. */
      if (outcome === "refused") toast.error(t("chat.ask.consentRequired"));
      else if (outcome === "failed") toast.error(t("chat.ask.error"));
    });
  };

  /* Run, then say what came back. The answer lands in the record's own Prompt
   * results section, so the toast names the outcome rather than repeating it —
   * and a run that produced nothing still says which absence it was, because
   * nothing retries. */
  const run = (prompt: SavedPrompt) => {
    if (promptTarget === null) return;
    onOpenChange(false);
    void runSavedPrompt(prompt.prompt_id, promptTarget).then((outcome) => {
      if (outcome.status === "missing") toast.error(t("prompts.run.missing"));
      else if (outcome.status === "failed")
        toast.error(t("prompts.run.failed"));
      else if (outcome.run.result.kind === "failed") {
        toast.error(t(promptFailureKeys[outcome.run.result.reason]));
      } else {
        toast.success(t("prompts.run.saved", { name: prompt.name }));
      }
    });
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        showCloseButton={false}
        /* One of the two modals that asks for the frost. The palette is
           chrome: a list of destinations and one field, no measured data on it
           anywhere, and styles/primitives.css names it by hand as the surface
           that opts in. Modals are solid by default; see `material` there. */
        material="glass"
        /* Sits high rather than centred — a palette is read against the top of
           the window, not its middle — so the kit's vertical centring is
           replaced outright instead of offset.

           A floating panel, not a modal sheet: `--radius-panel` and the glass
           shadow, over the dialog's own 18px and `--shadow-dialog`. */
        className="top-[max(12vh,64px)] translate-y-0 gap-0 overflow-hidden rounded-panel border-gray-alpha-400 bg-surface-raised p-0 shadow-[var(--glass-shadow)] sm:max-w-[560px]"
      >
        <DialogHeader className="sr-only">
          <DialogTitle>{t("commandPalette.open")}</DialogTitle>
          <DialogDescription>
            {t("commandPalette.placeholder")}
          </DialogDescription>
        </DialogHeader>
        {/* The input row's own divider is this field's focus indicator: it steps
            to the next border colour while the field holds focus. A ring around
            the only focusable element inside an already-modal palette is noise,
            so the app's default focus outline is suppressed here and the
            divider is the replacement indicator base.css asks any suppressor to
            draw. */}
        <Command
          loop
          filter={paletteFilter}
          value={selected}
          onValueChange={setSelected}
          className="bg-transparent **:data-[slot=command-input-wrapper]:h-12 **:data-[slot=command-input-wrapper]:border-gray-alpha-400 **:data-[slot=command-input-wrapper]:px-4 **:data-[slot=command-input-wrapper]:focus-within:border-gray-alpha-600"
        >
          <CommandInput
            value={query}
            onValueChange={setQuery}
            placeholder={t("commandPalette.placeholder")}
            className="text-[14px] leading-[20px] text-gray-1000 placeholder:text-gray-900 focus-visible:outline-none"
          />
          {/* Sized so the whole registry fits. The 340px this inherited from
              the old stylesheet is 29px short of the ten rows and two headings
              the palette actually has, so it always scrolled and always cut a
              row in half — the old build merely hid the seam behind its
              footer. A palette that truncates its own contents on first open
              is a broken interaction, not a tight one. */}
          <CommandList className="max-h-[min(60vh,440px)]">
            <CommandEmpty className="py-10 text-center text-[14px] text-gray-900">
              {notice ?? t("commandPalette.noResults")}
            </CommandEmpty>
            {sections.map((section) => (
              <CommandGroup
                key={section.group}
                heading={groupLabels[section.group]}
                className={GROUP}
              >
                {section.items.map((action) => (
                  <CommandItem
                    key={action.id}
                    value={action.label}
                    onSelect={() => {
                      /* Closed first, then run: a navigating action goes
                         through a view transition whose `flushSync` also
                         flushes this close, so the palette leaves inside the
                         same cross-fade as the route instead of lingering for
                         a frame on the far side of it. */
                      onOpenChange(false);
                      action.run();
                    }}
                    /* Rows are the content of this surface, so they take the
                       content tier. Shipping them at gray-900 was the mistake:
                       measured against the palette's own #0a0a0a it is 7.66:1
                       where gray-1000 is 16.91:1, so every row you came here
                       to read was at less than half the contrast the surface
                       it replaced gave them. gray-900 is for prose; a row you
                       are scanning to pick is not prose. The muted tier stays
                       where it belongs — the group headings. */
                    className={ROW}
                  >
                    <span className="min-w-0 truncate">{action.label}</span>
                  </CommandItem>
                ))}
              </CommandGroup>
            ))}
            {/* Run a prompt: one row per saved prompt, offered only while a
                record has said what it is. A section rather than a mode — a
                picker you have to enter is a second keystroke before the
                first letter, and cmdk already filters these by name. */}
            {promptTarget !== null && prompts.length > 0 && (
              <CommandGroup
                heading={t("commandPalette.runPrompt")}
                className={GROUP}
              >
                {prompts.map((prompt) => (
                  <CommandItem
                    key={prompt.prompt_id}
                    value={`prompt-run:${prompt.prompt_id}`}
                    keywords={[prompt.name]}
                    onSelect={() => run(prompt)}
                    className={ROW}
                  >
                    <span className="min-w-0 truncate">{prompt.name}</span>
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
            {results.map((section) => (
              <CommandGroup
                key={section.kind}
                heading={t(resultHeadingKeys[section.kind])}
                className={GROUP}
              >
                {section.rows.map((row) => (
                  <ResultRow
                    key={row.link}
                    row={row}
                    now={now}
                    onSelect={() => choose(row)}
                  />
                ))}
              </CommandGroup>
            ))}
            {notice !== null && <SearchNotice message={notice} />}
            {asking && (
              <CommandGroup className="p-1.5">
                <CommandItem value={ASK_VALUE} onSelect={ask} className={ROW}>
                  <span className="min-w-0 truncate">
                    {t("chat.ask.row", { query: query.trim() })}
                  </span>
                </CommandItem>
              </CommandGroup>
            )}
          </CommandList>
        </Command>
      </DialogContent>
    </Dialog>
  );
};
