// Builds mobile/SonaKeyboard/emoji.tsv, the emoji keyboard's list, from Unicode's
// emoji-test.txt and CLDR's English annotations (Unicode License v3, see
// mobile/SonaKeyboard/emoji-LICENSE.txt).
//
// Each line is `group<TAB>emoji<TAB>name<TAB>keywords<TAB>skin tones`: the group's index in
// the keyboard's category bar, the emoji, its CLDR short name, its search keywords joined by
// "|", and its five skin-tone forms joined by spaces when it has them. Lines keep Unicode's
// order, which is the order Apple's keyboard shows. Emoji newer than `newest` are left out
// so every one draws on the oldest iOS the app supports.
//
// Usage: bun mobile/scripts/build-emoji.ts <dir with emoji-test.txt, ann-en.json, ann-derived-en.json>
// Sources: https://unicode.org/Public/emoji/latest/emoji-test.txt and
// cldr-json/cldr-annotations-full/annotations/en/annotations.json and
// cldr-json/cldr-annotations-derived-full/annotationsDerived/en/annotations.json.

import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const newest = 16.0;
const groups = [
  "Smileys & Emotion",
  "People & Body",
  "Animals & Nature",
  "Food & Drink",
  "Activities",
  "Travel & Places",
  "Objects",
  "Symbols",
  "Flags",
];
const tones = ["1F3FB", "1F3FC", "1F3FD", "1F3FE", "1F3FF"];

const source = process.argv[2];
if (!source) throw new Error("usage: build-emoji.ts <source dir>");
type Annotation = { default?: string[]; tts?: string[] };
const annotations = new Map<string, Annotation>(
  Object.entries({
    ...JSON.parse(readFileSync(join(source, "ann-derived-en.json"), "utf8")).annotationsDerived.annotations,
    ...JSON.parse(readFileSync(join(source, "ann-en.json"), "utf8")).annotations.annotations,
  }),
);
const strip = (emoji: string) => emoji.replaceAll("\uFE0F", "");
const annotationFor = (emoji: string) => annotations.get(emoji) ?? annotations.get(strip(emoji));

type Entry = { group: number; emoji: string; name: string; keywords: string[]; tones: string[] };
const entries: Entry[] = [];
const byCodes = new Map<string, Entry>();
let group = -1;

for (const line of readFileSync(join(source, "emoji-test.txt"), "utf8").split("\n")) {
  const heading = line.match(/^# group: (.+)$/);
  if (heading) {
    group = groups.indexOf(heading[1]);
    continue;
  }
  const match = line.match(/^([0-9A-F ]+?)\s*; fully-qualified\s*# (\S+) E(\d+\.\d+) (.+)$/);
  if (!match || group < 0 || Number(match[3]) > newest) continue;
  const [, codes, emoji, , name] = match;
  const points = codes.split(" ");
  const toned = points.filter((point) => tones.includes(point));
  if (toned.length) {
    // A form with one skin tone throughout belongs to the plain emoji before it.
    if (new Set(toned).size !== 1) continue;
    const base = points.filter((point) => !tones.includes(point)).join(" ");
    const plain = byCodes.get(base) ?? byCodes.get(base.replace(/ FE0F/g, ""));
    if (plain && plain.tones.length < 5) plain.tones.push(emoji);
    continue;
  }
  const annotation = annotationFor(emoji);
  const keywords = [...new Set([...(annotation?.tts ?? []), ...(annotation?.default ?? []), name])]
    .map((keyword) => keyword.toLowerCase())
    .filter((keyword) => !keyword.includes("|"));
  const entry = { group, emoji, name: annotation?.tts?.[0] ?? name, keywords, tones: [] };
  entries.push(entry);
  byCodes.set(codes, entry);
  byCodes.set(codes.replace(/ FE0F/g, ""), entry);
}

const out = join(import.meta.dir, "..", "SonaKeyboard", "emoji.tsv");
writeFileSync(
  out,
  entries
    .map((entry) =>
      [
        entry.group,
        entry.emoji,
        entry.name,
        entry.keywords.join("|"),
        entry.tones.length === 5 ? entry.tones.join(" ") : "",
      ].join("\t"),
    )
    .join("\n") + "\n",
);
console.log(`${entries.length} emoji, ${entries.filter((entry) => entry.tones.length === 5).length} with skin tones`);
