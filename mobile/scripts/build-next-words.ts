// Builds the keyboard's English and Russian word data in mobile/SonaKeyboard/NextWords from
// Tatoeba's sentence exports (CC BY 2.0 FR, https://tatoeba.org/en/downloads):
//
// - <lang>.tsv, the next-word table. Each line is `context<TAB>word word word`: the words most
//   often seen after the context, best first. A context is one word, two words joined by a
//   space, or empty for the start of a sentence.
// - <lang>-words.txt, the language's common words, most common first, one per line, for
//   completions and spelling guesses where the phone's dictionary has none.
//
// Words that are capitalized most of the time in mid-sentence are names (Tatoeba's
// ubiquitous "Tom", "Том") and appear in neither, except "I". Only mid-sentence uses count:
// a sentence's first word is capitalized whatever it is, and in Russian "Том" opens a sentence
// three times as often as it appears inside one.
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
const commonWords = 50000;

const word = /[\p{L}]+(?:['’][\p{L}]+)*/gu;

for (const code of picked.length ? picked : [...languages.keys()]) {
  const lang = languages.get(code);
  if (!lang) throw new Error(`unknown language ${code}`);
  const text = readFileSync(join(source, `${code}.tsv`), "utf8");
  const unigrams = new Map<string, number>();
  const midSentence = new Map<string, number>();
  const capitalMid = new Map<string, number>();
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
        if (index > 0) {
          midSentence.set(key, (midSentence.get(key) ?? 0) + 1);
          if (/^\p{Lu}/u.test(tokens[index])) capitalMid.set(key, (capitalMid.get(key) ?? 0) + 1);
        }
        count(index === 0 ? "" : keys[index - 1], key);
        if (index > 1) count(`${keys[index - 2]} ${keys[index - 1]}`, key);
      });
    }
  }

  const isName = (key: string) =>
    key !== "i" && (capitalMid.get(key) ?? 0) > (midSentence.get(key) ?? 0) * 0.45;
  const display = (key: string) =>
    lang === "en" && (key === "i" || key.startsWith("i'")) ? "I" + key.slice(1) : key;

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

  const words = [...unigrams.entries()]
    .filter(([key, value]) => value >= minimumCount && (midSentence.get(key) ?? 0) > 0 && !isName(key))
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .slice(0, commonWords)
    .map(([key]) => display(key));
  writeFileSync(join(out, `${lang}-words.txt`), words.join("\n") + "\n");
  console.log(`${lang}: ${lines.length} contexts, ${words.length} words`);
}
