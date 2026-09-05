import { describe, expect, test } from "bun:test";
import fs from "fs";
import path from "path";
import { fileURLToPath } from "url";
import {
  parseTranslationBundle,
  type TranslationTree,
} from "./translationTree";
import i18next from "i18next";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SRC_ROOT = path.join(__dirname, "..");
const EN_MESSAGES = parseTranslationBundle(
  fs.readFileSync(
    path.join(SRC_ROOT, "i18n", "locales", "en", "translation.json"),
    "utf8",
  ),
);
const ENGLISH_PLURAL_CATEGORIES = new Intl.PluralRules("en").resolvedOptions()
  .pluralCategories;

const hasEnglishMessage = (key: string): boolean =>
  EN_MESSAGES.has(key) ||
  ENGLISH_PLURAL_CATEGORIES.some((category) =>
    EN_MESSAGES.has(`${key}_${category}`),
  );

/* Count keys whose plural family every locale has to resolve. Three entries
 * left this list with the feed's copy pass: the continuity pair became one
 * sentence ("Carried 2 open loops forward"), and the People list no longer
 * offers to link suggested meetings. The consent-panel briefing belongs here
 * because its open-loop count must select the locale's exact CLDR form. */
const RUNTIME_COUNT_KEYS = [
  "consentPanel.seriesBrief",
  "people.list.meetings",
  "people.review.meetingsBefore",
  "people.briefing.metCount",
  "people.briefing.metBefore",
  "overview.activity.streakAria",
  "settings.workflows.vocabularySuggestions.occurrences",
  "settings.workflows.vocabularySuggestions.meetings",
  "settings.workflows.outcomes.personLinks",
  "settings.workflows.outcomes.vocabularyCandidates",
  "settings.workflows.outcomes.documentLinks",
  "libraryV2.recordings",
  "libraryV2.words",
  "meetings.review.unresolvedSpeakers",
  "consentPanel.wrap.unresolvedSpeakers",
] as const;

/* French, Hindi, and Portuguese select the same CLDR form for zero and one.
 * These counters remain visible at zero, so a reader must see the supplied
 * count rather than a singular form that hard-codes 1. */
const ZERO_AND_ONE_SHARED_KEYS = [
  "consentPanel.wrap.unresolvedSpeakers",
  "learningV2.feed.evidence",
  "learningV2.outcomes.noticed",
  "meetings.review.unresolvedSpeakers",
  "overview.week.dictations",
  "overview.week.streak",
  "overview.week.words",
  "people.list.meetings",
  "people.review.meetingsBefore",
  "secureInput.blockedNoCulprit",
  "secureInput.blockedWithCulprit",
  "settings.history.emptyRecordings",
  "settings.workflows.outcomes.continuity",
  "settings.workflows.outcomes.personLinks",
  "settings.workflows.vocabularySuggestions.meetings",
  "settings.workflows.vocabularySuggestions.occurrences",
] as const;

const PLURAL_SAMPLE_COUNTS = [
  ...Array.from({ length: 201 }, (_, count) => count),
  0.1,
  0.2,
  1.1,
  1.2,
  2.1,
  2.2,
  3.1,
  10.1,
  100.1,
  1_000,
  10_000,
  100_000,
  1_000_000,
];

const sampleCount = (
  rules: Intl.PluralRules,
  category: Intl.LDMLPluralRule,
): number => {
  const count = PLURAL_SAMPLE_COUNTS.find(
    (candidate) => rules.select(candidate) === category,
  );
  if (count === undefined) {
    throw new Error(
      `No runtime sample found for plural category "${category}"`,
    );
  }
  return count;
};

/** Keys whose interpolation makes them unusable as static lookups. */
const DYNAMIC_KEYS = {
  "settings.history.receipts.engine": ["local", "cloud", "local_fallback"],
  "settings.history.receipts.source": ["microphone", "file", "legacy"],
  "settings.history.receipts.deliveryOutcome": [
    "delivered",
    "definitely_not_dispatched",
    "dispatched_but_unconfirmed",
    "dispatched_under_secure_input",
  ],
  "settingsV2.tabs": ["essentials", "advanced", "debug"],
  "settings.workflows.items": [
    "person_linking.name",
    "person_linking.description",
    "pre_meeting_briefing.name",
    "pre_meeting_briefing.description",
    "continuity.name",
    "continuity.description",
    "vocabulary_mining.name",
    "vocabulary_mining.description",
    "document_linking.name",
    "document_linking.description",
    /* Permanently enabled and absent from the configurable list, so no switch
     * names it and no description key exists for it — but its receipts reach
     * the Capture feed, and the feed reads the same `items.<id>.name` key every
     * other workflow does. */
    "meeting_activity.name",
  ],
  "settings.workflows.status": ["ok", "failed", "skipped"],
  /* The five-tab mode editor is gone: one screen plus one Advanced
   * disclosure, so `settings.modes.tabs` has no call site left. */
  "settings.modes.views": ["modes", "vocabulary"],
  "modesV2.rules.kinds": ["vocabulary", "snippet", "replacement", "emoji"],
  "modesV2.rules.kindHints": ["vocabulary", "snippet", "replacement", "emoji"],
  "modesV2.rules.placeholders": [
    "vocabulary.left",
    "vocabulary.right",
    "snippet.left",
    "snippet.right",
    "replacement.left",
    "replacement.right",
    "emoji.left",
    "emoji.right",
  ],
  "modesV2.rules.toggles": ["emoji", "snippet", "replacement"],
  "modesV2.rules.toggleErrors": [
    "spokenEdits",
    "emoji",
    "snippet",
    "replacement",
  ],
  "theme.options": ["system", "light", "dark"],
};

const walk = (dir: string): string[] => {
  const files: string[] = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...walk(full));
    } else if (entry.name.endsWith(".ts") || entry.name.endsWith(".tsx")) {
      files.push(full);
    }
  }
  return files;
};

const findTranslationKeys = (): Set<string> => {
  const keys = new Set<string>();
  const keyPattern = /\bt\(\s*(['"`])([^'"`]+)\1\s*[,)]/g;
  for (const file of walk(SRC_ROOT)) {
    if (file.endsWith(".test.ts") || file.endsWith(".test.tsx")) continue;
    const source = fs.readFileSync(file, "utf8");
    for (const match of source.matchAll(keyPattern)) {
      keys.add(match[2]);
    }
  }
  return keys;
};

/* Every dotted string src mentions, and every namespace it builds a key under.
 *
 * Deliberately wider than the `t(` pattern above: a key reaches the screen
 * through a lookup table as often as through a call — `workflowCatalogue.ts`
 * maps workflow ids to key strings, `MeetingDetectionSettings` keeps its
 * suppression copy in a `satisfies Record<...>` table — and a sweep that only
 * saw `t(` would call all of those orphaned. So a bare literal counts, and a
 * literal or template head ending in a dot marks its whole namespace reached:
 * `"settingsV2.apps.names." + app.id` and
 * `` `settings.models.cloud.errors.${err}` `` are the same claim about a
 * namespace, written two ways. */
const findReferencedCopy = () => {
  const literals = new Set<string>();
  const namespaces = new Set<string>();
  /* A single-, double- or back-quoted run with no quote or newline in it. A
   * template literal's static head stops at its first `${`, which is exactly
   * the prefix a dynamic key is built from — a dot for a nested namespace
   * (`settingsV2.tabs.`), an underscore for a hand-selected plural category
   * (`secureInput.blockedNoCulprit_`). */
  const literalPattern = /(['"`])([A-Za-z0-9_.]+)(?:\1|\$\{)/g;
  for (const file of walk(SRC_ROOT)) {
    if (file.endsWith(".test.ts") || file.endsWith(".test.tsx")) continue;
    const source = fs.readFileSync(file, "utf8");
    for (const [, , text] of source.matchAll(literalPattern)) {
      if (!text.includes(".")) continue;
      if (text.endsWith(".") || text.endsWith("_")) namespaces.add(text);
      else literals.add(text);
    }
  }
  return { literals, namespaces: Array.from(namespaces) };
};

describe("English translation fallback", () => {
  const usedKeys = findTranslationKeys();

  test("every static t() key used in src resolves in the en bundle", () => {
    const missing: string[] = [];
    for (const key of usedKeys) {
      if (key.includes("${")) continue;
      if (!hasEnglishMessage(key)) {
        missing.push(key);
      }
    }
    expect(missing).toEqual([]);
  });

  test("every dynamic t() namespace value resolves in the en bundle", () => {
    const missing: string[] = [];
    for (const [namespace, values] of Object.entries(DYNAMIC_KEYS)) {
      for (const value of values) {
        const key = `${namespace}.${value}`;
        if (!EN_MESSAGES.has(key)) {
          missing.push(key);
        }
      }
    }
    expect(missing).toEqual([]);
  });

  test("no en translation value is a raw key leak", () => {
    const leaks: string[] = [];
    for (const key of usedKeys) {
      if (key.includes("${")) continue;
      if (EN_MESSAGES.get(key) === key) {
        leaks.push(key);
      }
    }
    expect(leaks).toEqual([]);
  });
});

const REFERENCED_COPY = findReferencedCopy();
const DYNAMIC_NAMESPACES = Object.keys(DYNAMIC_KEYS).map(
  (namespace) => `${namespace}.`,
);

/* The other direction: a catalogue key nothing renders.
 *
 * The parity and plural checks above ask whether every locale can answer a
 * key. Neither asks whether anyone is asking, so cutovers leave leaves behind
 * — `settings.hub.*`, `sidebar.general`, nine `modelSelector.*`, the copy of
 * five deleted rows — each multiplied by twenty-four locale files, each still
 * shipped to translators. A dead key is cheap on its own and expensive by the
 * file: it is a paragraph a volunteer translates for a surface that does not
 * exist.
 *
 * `unreachedCopy.json` is the debt this check was added on top of: keys with no
 * reference in `src` that were NOT individually verified as safe to delete.
 * Some are genuinely dead (`settings.modes.tabs.*` — the five-tab mode editor
 * is gone); others may be read somewhere this sweep cannot see, which is
 * exactly why they were not deleted on a regex's word. The list may only
 * shrink: a key removed from the catalogue must be removed from it, and a new
 * key must be referenced from `src` rather than added here. */
const UNREACHED_COPY: string[] = JSON.parse(
  fs.readFileSync(path.join(SRC_ROOT, "i18n", "unreachedCopy.json"), "utf8"),
);

describe("the English catalogue", () => {
  const reachedBy = (key: string): boolean => {
    const { literals, namespaces } = REFERENCED_COPY;
    /* A plural family is reached through its base key: `t(k, {count})` never
     * names `k_other`. A caller that selects the category itself instead leaves
     * a `..._` prefix, so the raw key is offered to the namespaces as well. */
    const base = key.replace(/_(zero|one|two|few|many|other)$/, "");
    if (literals.has(base) || literals.has(key)) return true;
    return [...namespaces, ...DYNAMIC_NAMESPACES].some(
      (prefix) => base.startsWith(prefix) || key.startsWith(prefix),
    );
  };

  test("ships no key that nothing in src can reach", () => {
    const orphans = Array.from(EN_MESSAGES.keys())
      .filter((key) => !reachedBy(key) && !UNREACHED_COPY.includes(key))
      .sort();

    expect(orphans).toEqual([]);
  });

  test("carries every key the unreached list still claims", () => {
    /* Otherwise the list outlives the debt and starts hiding the next orphan:
     * a deleted key left in here is a name nothing can ever justify again. */
    const stale = UNREACHED_COPY.filter(
      (key) => !EN_MESSAGES.has(key) || reachedBy(key),
    ).sort();

    expect(stale).toEqual([]);
  });
});

describe("runtime plural resolution", () => {
  const localesRoot = path.join(SRC_ROOT, "i18n", "locales");
  const locales = fs
    .readdirSync(localesRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();

  test("every new count key resolves the locale-selected CLDR category", async () => {
    for (const locale of locales) {
      // SAFETY: translation.json files hold the nested string tree that
      // TranslationTree names; the schema is enforced by the parity checks
      // above, so decoding straight into the owning contract is honest.
      const bundle = JSON.parse(
        fs.readFileSync(
          path.join(localesRoot, locale, "translation.json"),
          "utf8",
        ),
      ) as TranslationTree;
      const instance = i18next.createInstance();
      await instance.init({
        lng: locale,
        fallbackLng: false,
        resources: { [locale]: { translation: bundle } },
      });
      const rules = new Intl.PluralRules(locale);

      for (const category of rules.resolvedOptions().pluralCategories) {
        const count = sampleCount(rules, category);
        for (const key of RUNTIME_COUNT_KEYS) {
          const details = instance.t(key, {
            count,
            returnDetails: true,
          });
          expect(details.exactUsedKey).toBe(`${key}_${category}`);
        }
      }
    }
  });

  test("zero stays distinct from one when both select one", async () => {
    for (const locale of ["fr", "hi", "pt"]) {
      // SAFETY: parity checks above validate this locale's nested string tree
      // before it reaches this runtime i18next probe.
      const bundle = JSON.parse(
        fs.readFileSync(
          path.join(localesRoot, locale, "translation.json"),
          "utf8",
        ),
      ) as TranslationTree;
      const instance = i18next.createInstance();
      await instance.init({
        lng: locale,
        fallbackLng: false,
        resources: { [locale]: { translation: bundle } },
      });

      const rules = new Intl.PluralRules(locale);
      expect(rules.select(0)).toBe("one");
      expect(rules.select(1)).toBe("one");

      for (const key of ZERO_AND_ONE_SHARED_KEYS) {
        const options = { days: 7, name: "Sona" };
        expect(instance.t(key, { ...options, count: 0 })).not.toBe(
          instance.t(key, { ...options, count: 1 }),
        );
      }
    }
  });
});
