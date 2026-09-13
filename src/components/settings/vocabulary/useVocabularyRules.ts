import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  commands,
  type EmojiReplacement,
  type VocabularyEntry,
} from "@/bindings";
import {
  mergeAppliedCsv,
  resolveRefreshDraft,
  spokenMatchKey,
} from "@/lib/vocabularyDraft";
import {
  getTextReplacements,
  resetTextReplacements,
  saveTextReplacements,
  setSpokenEditsEnabled,
  setTextReplacementsEnabled,
  type ReplacementRule,
} from "@/lib/powerPackApi";
import { useSettings } from "@/hooks/useSettings";
import { addVocabularyCandidate } from "../custom-words/meetingVocabulary";
import {
  deleteSnippet,
  draftSnippet,
  listSnippets,
  setSnippetEnabled,
  setSnippetsEnabled,
  triggerKey,
  upsertSnippet,
  type Snippet,
} from "./snippetsApi";
import {
  GLOBAL_VOCABULARY_SCOPE,
  useVocabularyImport,
  type VocabularyImportState,
} from "./useVocabularyImport";

/**
 * The four text-rule stores, behind one list.
 *
 * Each store keeps its own persisted shape and its own commands — a spelling
 * is a `custom_words` entry, a shortcut is a `snippets` record, a rewrite is a
 * `replacements` rule, an emoji is an `emoji_replacements` pair — and nothing
 * here migrates or merges the data. What is merged is the surface: one row
 * shape, one add flow, one owner for "which store does this row belong to".
 *
 * Every store's write takes and answers with the whole list, so a row commits
 * on blur and the answer replaces local state. That is why there is no Save
 * button and no unsaved-changes state: there is nothing a save could batch
 * that the backend does not already take whole.
 */

export const RULE_KINDS = [
  "vocabulary",
  "snippet",
  "replacement",
  "emoji",
] as const;

export type RuleKind = (typeof RULE_KINDS)[number];

export interface MergedRule {
  /** `${kind}:${address}` — the row's React key, and nothing more. */
  id: string;
  kind: RuleKind;
  /** Which store owns this row, and where in it. */
  address: RuleAddress;
  /** What the person says. */
  left: string;
  /** What Sona writes. */
  right: string;
  /** `null` for stores with no per-rule switch. */
  enabled: boolean | null;
}

/**
 * Which store a row belongs to, and where in it.
 *
 * Three of the four stores address a rule by its position in the list they
 * take whole; snippets have their own record IDs. Naming that as one parsed
 * union is what keeps a `replacement` row from ever writing into
 * `custom_words`: every mutation switches on this and nothing re-reads the
 * string.
 */
export type RuleAddress =
  | { kind: "vocabulary"; index: number }
  | { kind: "emoji"; index: number }
  | { kind: "replacement"; index: number }
  | { kind: "snippet"; snippetId: string };

const EMPTY_ENTRIES: VocabularyEntry[] = [];
const EMPTY_EMOJI: EmojiReplacement[] = [];

export const ruleId = (kind: RuleKind, address: string | number): string =>
  `${kind}:${address}`;

export interface VocabularyRulesState extends VocabularyImportState {
  rules: MergedRule[];
  loading: boolean;
  busy: boolean;
  /** A failed read or write, with the retry that produced it. */
  failure: { message: string; retry: () => void } | null;
  /** Why this row cannot be written yet, keyed by row id. */
  problems: Record<string, string>;
  savedVocabularyCount: number;

  editRule: (rule: MergedRule, side: "left" | "right", value: string) => void;
  commitRule: (rule: MergedRule) => void;
  removeRule: (rule: MergedRule) => void;
  toggleRule: (rule: MergedRule, enabled: boolean) => void;
  addRule: (kind: RuleKind, left: string, right: string) => void;
  /** One accepted meeting suggestion, added to the spelling store. */
  addSuggestion: (text: string) => void;
  /** Live vocabulary entries, for the suggestion filter. */
  vocabularyEntries: readonly VocabularyEntry[];

  spokenEditsEnabled: boolean;
  emojiEnabled: boolean;
  snippetsEnabled: boolean;
  replacementsEnabled: boolean;
  setSpokenEdits: (enabled: boolean) => void;
  setEmoji: (enabled: boolean) => void;
  setSnippets: (enabled: boolean) => void;
  setReplacements: (enabled: boolean) => void;
  restoreDefaultRewrites: () => void;
}

export const useVocabularyRules = (): VocabularyRulesState => {
  const { t } = useTranslation();
  const { settings, isLoading, refreshSettings } = useSettings();

  const savedEntries = settings?.custom_words ?? EMPTY_ENTRIES;
  const savedEmoji = settings?.emoji_replacements ?? EMPTY_EMOJI;
  const syncedEntriesRef = useRef(savedEntries);
  const syncedEmojiRef = useRef(savedEmoji);

  const [entries, setEntries] = useState<VocabularyEntry[]>(savedEntries);
  const [emoji, setEmojiEntries] = useState<EmojiReplacement[]>(savedEmoji);
  const [snippets, setSnippetList] = useState<Snippet[]>([]);
  const [snippetDrafts, setSnippetDrafts] = useState<
    Record<string, { trigger: string; expansion: string }>
  >({});
  const [rewrites, setRewrites] = useState<ReplacementRule[]>([]);
  const [listsLoading, setListsLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<{
    message: string;
    retry: () => void;
  } | null>(null);

  /* A refresh may replace the local list only when nothing local has diverged
   * since the last synced snapshot; otherwise the person is mid-edit and an
   * unrelated settings refresh would discard the row under their cursor. */
  useEffect(() => {
    const previousSaved = syncedEntriesRef.current;
    setEntries((current) =>
      resolveRefreshDraft(current, previousSaved, savedEntries),
    );
    syncedEntriesRef.current = savedEntries;
  }, [savedEntries]);

  useEffect(() => {
    const previousSaved = syncedEmojiRef.current;
    setEmojiEntries((current) =>
      resolveRefreshDraft(current, previousSaved, savedEmoji),
    );
    syncedEmojiRef.current = savedEmoji;
  }, [savedEmoji]);

  const loadLists = useCallback(async () => {
    setListsLoading(true);
    setFailure(null);
    try {
      const [savedSnippets, savedRewrites] = await Promise.all([
        listSnippets(),
        getTextReplacements(),
      ]);
      setSnippetList(savedSnippets);
      setRewrites(savedRewrites);
    } catch (loadError) {
      setFailure({
        message: String(loadError),
        retry: () => void loadLists(),
      });
    } finally {
      setListsLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadLists();
  }, [loadLists]);

  /* Writes are serialized: every command answers with the authoritative list,
   * so two in flight at once would let the slower answer overwrite the newer
   * one. */
  const runWrite = useCallback(
    async (write: () => Promise<void>, retry: () => void) => {
      setBusy(true);
      setFailure(null);
      try {
        await write();
      } catch (writeError) {
        setFailure({ message: String(writeError), retry });
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const writeEntries = useCallback(
    (next: readonly VocabularyEntry[]) =>
      void runWrite(
        async () => {
          const result = await commands.updateVocabularyEntries(
            GLOBAL_VOCABULARY_SCOPE,
            [...next],
          );
          if (result.status !== "ok") throw new Error(String(result.error));
          setEntries(result.data);
          await refreshSettings();
        },
        () => writeEntries(next),
      ),
    [refreshSettings, runWrite],
  );

  const writeEmoji = useCallback(
    (next: readonly EmojiReplacement[]) =>
      void runWrite(
        async () => {
          const result = await commands.updateEmojiReplacements([...next]);
          if (result.status !== "ok") throw new Error(String(result.error));
          setEmojiEntries(result.data);
          await refreshSettings();
        },
        () => writeEmoji(next),
      ),
    [refreshSettings, runWrite],
  );

  const writeSnippets = useCallback(
    (write: () => Promise<Snippet[]>) =>
      void runWrite(
        async () => setSnippetList(await write()),
        () => writeSnippets(write),
      ),
    [runWrite],
  );

  const writeRewrites = useCallback(
    (next: readonly ReplacementRule[]) =>
      void runWrite(
        async () => setRewrites(await saveTextReplacements([...next])),
        () => writeRewrites(next),
      ),
    [runWrite],
  );

  /* The backend adds the CSV rows to the persisted list and answers with that
   * list. Local rows the CSV does not define are not in that answer, so merge
   * them back instead of silently discarding them. */
  const applyCsvRows = useCallback(
    async (rows: VocabularyEntry[]) => {
      setEntries((current) => mergeAppliedCsv(current, rows));
      await refreshSettings();
    },
    [refreshSettings],
  );

  const csvImport = useVocabularyImport({
    runWrite,
    onApplied: applyCsvRows,
  });

  const snippetDraftFor = (snippet: Snippet) =>
    snippetDrafts[snippet.id] ?? {
      trigger: snippet.trigger,
      expansion: snippet.expansion,
    };

  /* The rows, grouped by store in a fixed order. Each row carries its kind, so
   * grouping is a reading aid rather than a second source of truth. */
  const rules: MergedRule[] = [];
  for (const [index, entry] of entries.entries()) {
    rules.push({
      id: ruleId("vocabulary", index),
      address: { kind: "vocabulary", index },
      kind: "vocabulary",
      left: entry.spoken,
      right: entry.written,
      enabled: null,
    });
  }
  for (const snippet of snippets) {
    const draft = snippetDraftFor(snippet);
    rules.push({
      id: ruleId("snippet", snippet.id),
      address: { kind: "snippet", snippetId: snippet.id },
      kind: "snippet",
      left: draft.trigger,
      right: draft.expansion,
      enabled: snippet.enabled,
    });
  }
  for (const [index, rule] of rewrites.entries()) {
    rules.push({
      id: ruleId("replacement", index),
      address: { kind: "replacement", index },
      kind: "replacement",
      left: rule.spoken,
      right: rule.written,
      enabled: rule.enabled,
    });
  }
  for (const [index, pair] of emoji.entries()) {
    rules.push({
      id: ruleId("emoji", index),
      address: { kind: "emoji", index },
      kind: "emoji",
      left: pair.spoken,
      right: pair.written,
      enabled: null,
    });
  }

  const incompleteText = t("modesV2.rules.errors.incomplete");
  const duplicateText = t("modesV2.rules.errors.duplicate");

  /* Exactly what the backend would refuse, named on the row that would be
   * refused: an incomplete pair is dropped on write, and a colliding key
   * rejects the whole list. Checking here turns a silent deletion into a
   * sentence under the field. */
  const problems: Record<string, string> = {};
  for (const rule of rules) {
    if (rule.left.trim() === "" || rule.right.trim() === "") {
      problems[rule.id] = incompleteText;
      continue;
    }
    const collides = rules.some((other) => {
      if (other.id === rule.id || other.kind !== rule.kind) return false;
      return rule.kind === "snippet"
        ? triggerKey(other.left) === triggerKey(rule.left)
        : spokenMatchKey(other.left) === spokenMatchKey(rule.left);
    });
    if (collides) problems[rule.id] = duplicateText;
  }

  const editRule = (
    { address }: MergedRule,
    side: "left" | "right",
    value: string,
  ) => {
    const spokenField = side === "left" ? "spoken" : "written";
    switch (address.kind) {
      case "vocabulary":
        setEntries((current) =>
          current.map((entry, row) =>
            row === address.index ? { ...entry, [spokenField]: value } : entry,
          ),
        );
        return;
      case "emoji":
        setEmojiEntries((current) =>
          current.map((entry, row) =>
            row === address.index ? { ...entry, [spokenField]: value } : entry,
          ),
        );
        return;
      case "replacement":
        setRewrites((current) =>
          current.map((rule, row) =>
            row === address.index ? { ...rule, [spokenField]: value } : rule,
          ),
        );
        return;
      case "snippet": {
        const snippet = snippets.find(
          (candidate) => candidate.id === address.snippetId,
        );
        if (!snippet) return;
        setSnippetDrafts((current) => ({
          ...current,
          [address.snippetId]: {
            ...snippetDraftFor(snippet),
            [side === "left" ? "trigger" : "expansion"]: value,
          },
        }));
      }
    }
  };

  const commitRule = (rule: MergedRule) => {
    if (busy || problems[rule.id] !== undefined) return;
    const { address } = rule;
    switch (address.kind) {
      case "vocabulary":
        writeEntries(entries);
        return;
      case "emoji":
        writeEmoji(emoji);
        return;
      case "replacement":
        writeRewrites(rewrites);
        return;
      case "snippet": {
        const snippet = snippets.find(
          (candidate) => candidate.id === address.snippetId,
        );
        if (!snippet) return;
        const draft = snippetDraftFor(snippet);
        /* Snippets are the one store written per record, so an untouched row
         * has nothing to send and a blur must not bump `updated_at`. */
        if (
          draft.trigger.trim() === snippet.trigger &&
          draft.expansion.trim() === snippet.expansion
        ) {
          return;
        }
        writeSnippets(() =>
          upsertSnippet({
            ...snippet,
            trigger: draft.trigger.trim(),
            expansion: draft.expansion.trim(),
          }),
        );
      }
    }
  };

  const removeRule = ({ address }: MergedRule) => {
    switch (address.kind) {
      case "vocabulary":
        writeEntries(entries.filter((_, row) => row !== address.index));
        return;
      case "emoji":
        writeEmoji(emoji.filter((_, row) => row !== address.index));
        return;
      case "replacement":
        writeRewrites(rewrites.filter((_, row) => row !== address.index));
        return;
      case "snippet":
        writeSnippets(() => deleteSnippet(address.snippetId));
    }
  };

  const toggleRule = ({ address }: MergedRule, enabled: boolean) => {
    if (address.kind === "replacement") {
      writeRewrites(
        rewrites.map((rule, row) =>
          row === address.index ? { ...rule, enabled } : rule,
        ),
      );
      return;
    }
    /* The other three stores have no per-rule switch, so their rows never
     * render one and nothing can reach this with their ids. */
    if (address.kind === "snippet") {
      writeSnippets(() => setSnippetEnabled(address.snippetId, enabled));
    }
  };

  const addRule = (kind: RuleKind, left: string, right: string) => {
    const spoken = left.trim();
    const written = kind === "replacement" ? right : right.trim();
    if (spoken === "" || written === "") return;
    switch (kind) {
      case "vocabulary":
        writeEntries([...entries, { spoken, written }]);
        return;
      case "emoji":
        writeEmoji([...emoji, { spoken, written }]);
        return;
      case "replacement":
        writeRewrites([...rewrites, { spoken, written, enabled: true }]);
        return;
      case "snippet":
        writeSnippets(() => upsertSnippet(draftSnippet(spoken, written)));
    }
  };

  const addSuggestion = (text: string) =>
    writeEntries(addVocabularyCandidate(entries, text));

  const toggleSetting = (
    write: () => Promise<void>,
    errorKey: string,
    retry: () => void,
  ) =>
    void runWrite(async () => {
      try {
        await write();
        await refreshSettings();
      } catch (toggleError) {
        toast.error(t(errorKey));
        throw toggleError;
      }
    }, retry);

  const setSpokenEdits = (enabled: boolean) =>
    toggleSetting(
      () => setSpokenEditsEnabled(enabled),
      "modesV2.rules.toggleErrors.spokenEdits",
      () => setSpokenEdits(enabled),
    );

  const setEmoji = (enabled: boolean) =>
    toggleSetting(
      async () => {
        const result = await commands.updateEmojiReplacementsEnabled(enabled);
        if (result.status !== "ok") throw new Error(String(result.error));
      },
      "modesV2.rules.toggleErrors.emoji",
      () => setEmoji(enabled),
    );

  const setSnippets = (enabled: boolean) =>
    toggleSetting(
      () => setSnippetsEnabled(enabled),
      "modesV2.rules.toggleErrors.snippet",
      () => setSnippets(enabled),
    );

  const setReplacements = (enabled: boolean) =>
    toggleSetting(
      () => setTextReplacementsEnabled(enabled),
      "modesV2.rules.toggleErrors.replacement",
      () => setReplacements(enabled),
    );

  const restoreDefaultRewrites = () =>
    void runWrite(
      async () => setRewrites(await resetTextReplacements()),
      restoreDefaultRewrites,
    );

  return {
    rules,
    loading: isLoading || listsLoading,
    busy,
    failure,
    problems,
    savedVocabularyCount: savedEntries.length,

    editRule,
    commitRule,
    removeRule,
    toggleRule,
    addRule,
    addSuggestion,
    vocabularyEntries: entries,

    spokenEditsEnabled: settings?.spoken_edits_enabled ?? false,
    emojiEnabled: settings?.emoji_replacements_enabled ?? false,
    snippetsEnabled: settings?.snippets_enabled ?? true,
    replacementsEnabled: settings?.replacements_enabled ?? true,
    setSpokenEdits,
    setEmoji,
    setSnippets,
    setReplacements,
    restoreDefaultRewrites,

    ...csvImport,
  };
};
