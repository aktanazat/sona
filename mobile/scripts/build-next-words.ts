// Builds mobile/SonaKeyboard/NextWords/<lang>.tsv, the keyboard's English and Russian next-word tables, from
// Tatoeba's sentence exports (CC BY 2.0 FR, https://tatoeba.org/en/downloads).
//
// Each line is `context<TAB>word word word`: the words most often seen after the context,
// best first. A context is one word, two words joined by a space, or empty for the start
// of a sentence. Words that are capitalized in mid-sentence most of the time (names such
// as Tatoeba's ubiquitous "Tom") are never predicted, except "I".
//
// Usage: bun mobile/scripts/build-next-words.ts <dir with <code>.tsv exports> [codes...]
// The exports are https://downloads.tatoeba.org/exports/per_language/<code>/<code>_sentences.tsv.bz2,
// decompressed.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const languages = new Map([
  ["eng", "en"],
  ["rus", "ru"],
]);

const [source, ...picked] = process.argv.slice(2);
if (!source) throw new Error("usage: build-next-words.ts <exports dir> [codes...]");
const out = join(import.meta.dir, "..", "SonaKeyboard", "NextWords");
mkdirSync(out, { recursive: true });

const perContext = 3;
const bigramContexts = 6000;
const trigramContexts = 8000;
const minimumCount = 3;
const tatoebaNames = new Set(["tom", "mary", "maria", "marie", "john", "jim", "ken", "tomás", "tomas", "mari", "toms", "tom's", "mary's", "sami", "layla", "mennad", "baya", "ziri", "yanni", "skura"]);

const word = /[\p{L}]+(?:['’][\p{L}]+)*/gu;

for (const code of picked.length ? picked : [...languages.keys()]) {
  const lang = languages.get(code);
  if (!lang) throw new Error(`unknown language ${code}`);
  const text = readFileSync(join(source, `${code}.tsv`), "utf8");
  const unigrams = new Map<string, number>();
  const capitalMid = new Map<string, number>();
  const forms = new Map<string, Map<string, number>>();
  const follows = new Map<string, Map<string, number>>();

  const count = (context: string, next: string) => {
    let row = follows.get(context);
    if (!row) follows.set(context, (row = new Map()));
    row.set(next, (row.get(next) ?? 0) + 1);
  };

  for (const line of text.split("\n")) {
    const sentence = line.split("\t")[2];
    if (!sentence) continue;
    // A clause ends at sentence punctuation, so "Yes. I" does not make "yes" predict "I".
    for (const clause of sentence.split(/[.!?;:…"“”«»()]/)) {
      const tokens = clause.match(word) ?? [];
      const keys = tokens.map((token) => token.replace("’", "'").toLowerCase());
      keys.forEach((key, index) => {
        unigrams.set(key, (unigrams.get(key) ?? 0) + 1);
        const form = tokens[index].replace("’", "'");
        let seen = forms.get(key);
        if (!seen) forms.set(key, (seen = new Map()));
        seen.set(form, (seen.get(form) ?? 0) + 1);
        if (index > 0 && /^\p{Lu}/u.test(form)) capitalMid.set(key, (capitalMid.get(key) ?? 0) + 1);
        count(index === 0 ? "" : keys[index - 1], key);
        if (index > 1) count(`${keys[index - 2]} ${keys[index - 1]}`, key);
      });
    }
  }

  // German capitalizes every noun, so there only Tatoeba's recurring characters are dropped.
  const isName = (key: string) =>
    tatoebaNames.has(key) ||
    (lang !== "de" && key !== "i" && (capitalMid.get(key) ?? 0) > (unigrams.get(key) ?? 0) * 0.45);
  const display = (key: string) => {
    if (lang === "en" && (key === "i" || key.startsWith("i'"))) return "I" + key.slice(1);
    const seen = forms.get(key);
    if (!seen) return key;
    // The lowercase form unless the word is nearly always capitalized (German nouns).
    const lower = seen.get(key) ?? 0;
    const total = [...seen.values()].reduce((sum, value) => sum + value, 0);
    if (lower >= total * 0.3) return key;
    return [...seen.entries()].sort((a, b) => b[1] - a[1])[0][0];
  };

  const best = (row: Map<string, number>) =>
    [...row.entries()]
      .filter(([key, value]) => value >= minimumCount && !isName(key))
      .sort((a, b) => b[1] - a[1])
      .slice(0, perContext)
      .map(([key]) => display(key));

  const contexts = [...follows.entries()].map(([context, row]) => ({
    context,
    total: [...row.values()].reduce((sum, value) => sum + value, 0),
    row,
  }));
  const singles = contexts
    .filter(({ context }) => !context.includes(" ") && !isName(context))
    .sort((a, b) => b.total - a.total)
    .slice(0, bigramContexts);
  const pairs = contexts
    .filter(({ context }) => context.includes(" ") && !context.split(" ").some(isName))
    .sort((a, b) => b.total - a.total)
    .slice(0, trigramContexts);

  const lines: string[] = [];
  for (const { context, row } of [...singles, ...pairs]) {
    const words = best(row);
    if (words.length) lines.push(`${context}\t${words.join(" ")}`);
  }
  lines.sort();
  writeFileSync(join(out, `${lang}.tsv`), lines.join("\n") + "\n");
  console.log(`${lang}: ${lines.length} contexts`);
}
