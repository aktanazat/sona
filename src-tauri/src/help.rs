//! "How do I…" answers about Sona from the docs it ships with: the README's
//! user-facing sections and the keyboard shortcut sheet. Nothing here reads
//! the corpus, so no surface asks for consent before searching it.

use crate::cli::CliArgs;
use serde::Serialize;
use specta::Type;
use std::sync::LazyLock;

const README: &str = include_str!("../../README.md");
/// The shortcut sheet stays the one list of shortcuts; this reads its groups.
const SHORTCUT_SHEET: &str = include_str!("../../macos/Sona/ShortcutSheet.swift");

const MAX_TEXT: usize = 1200;
const TITLE_WEIGHT: usize = 3;
/// Two query words side by side in a section count as much as a title hit.
const PAIR_WEIGHT: usize = 3;
const CLI_DEFAULT_LIMIT: usize = 5;
const CLI_MAX_LIMIT: usize = 20;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "can", "do", "does", "for", "from", "how", "i", "in", "is", "it",
    "me", "my", "of", "on", "or", "the", "this", "that", "to", "what", "when", "where", "with",
    "you",
];

#[derive(Clone, Debug, Serialize, Type)]
pub(crate) struct HelpSection {
    /// "guide" for the README, "shortcuts" for the shortcut sheet.
    pub(crate) source: String,
    pub(crate) title: String,
    pub(crate) text: String,
}

static SECTIONS: LazyLock<Vec<HelpSection>> = LazyLock::new(|| {
    let mut sections = guide_sections(README);
    sections.extend(shortcut_sections(SHORTCUT_SHEET));
    sections
});

/// The sections that best answer `words`, best first; sections that score
/// the same keep the order they are written in. Without a meaningful word,
/// the first `limit` guide sections.
pub(crate) fn search(words: &str, limit: usize) -> Vec<HelpSection> {
    let wanted = query_words(words);
    if wanted.is_empty() {
        return SECTIONS
            .iter()
            .filter(|section| section.source == "guide")
            .take(limit)
            .cloned()
            .collect();
    }
    let mut scored: Vec<(usize, &HelpSection)> = SECTIONS
        .iter()
        .filter_map(|section| {
            let score = score(section, &wanted);
            (score > 0).then_some((score, section))
        })
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, section)| section.clone())
        .collect()
}

#[tauri::command]
#[specta::specta]
pub fn help_search(words: String, limit: Option<usize>) -> Vec<HelpSection> {
    search(&words, limit.unwrap_or(3))
}

/// `sona --help-search WORDS... [--limit N] [--json]`: the sections as text,
/// or one `{"sections":[…]}` value. A limit outside 1 to 20 exits 2.
pub(crate) fn run_cli(args: &CliArgs) -> i32 {
    let limit = args.limit.unwrap_or(CLI_DEFAULT_LIMIT);
    if !(1..=CLI_MAX_LIMIT).contains(&limit) {
        let message = format!("--limit must be between 1 and {CLI_MAX_LIMIT}.");
        if args.json {
            eprintln!(
                "{}",
                serde_json::json!({ "error": "invalid_request", "message": message })
            );
        } else {
            eprintln!("error: {message}");
        }
        return 2;
    }
    let words = args.help_search.join(" ");
    let sections = search(&words, limit);
    if args.json {
        println!("{}", serde_json::json!({ "sections": sections }));
    } else if sections.is_empty() {
        println!("Nothing in Sona's help matches \"{}\".", words.trim());
    } else {
        let blocks: Vec<String> = sections
            .iter()
            .map(|section| format!("{} ({})\n{}", section.title, section.source, section.text))
            .collect();
        println!("{}", blocks.join("\n\n"));
    }
    0
}

fn query_words(words: &str) -> Vec<String> {
    tokens(words)
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .collect()
}

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
}

/// A word of three letters or more also matches the longer words it starts,
/// so "shortcut" finds "shortcuts"; a shorter one must match whole.
fn matches(word: &str, token: &str) -> bool {
    if word.chars().count() >= 3 {
        token.starts_with(word)
    } else {
        token == word
    }
}

fn score(section: &HelpSection, wanted: &[String]) -> usize {
    let title: Vec<String> = tokens(&section.title).collect();
    let text: Vec<String> = tokens(&section.text).collect();
    let mut score = 0;
    for word in wanted {
        if title.iter().any(|token| matches(word, token)) {
            score += TITLE_WEIGHT;
        }
        if text.iter().any(|token| matches(word, token)) {
            score += 1;
        }
    }
    for pair in wanted.windows(2) {
        if text
            .windows(2)
            .any(|side| matches(&pair[0], &side[0]) && matches(&pair[1], &side[1]))
        {
            score += PAIR_WEIGHT;
        }
    }
    score
}

/// The README's "What Sona does" subsections and its "Command line" section.
/// A subsection longer than one section's text is split between paragraphs
/// and list items into numbered parts.
fn guide_sections(readme: &str) -> Vec<HelpSection> {
    let mut sections = Vec::new();
    let mut chapter = "";
    let mut heading: Option<String> = None;
    let mut body: Vec<&str> = Vec::new();
    let mut in_code = false;
    for line in readme.lines() {
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        let new_heading = if in_code {
            None
        } else if let Some(title) = line.strip_prefix("## ") {
            chapter = title.trim();
            Some((chapter == "Command line").then(|| chapter.to_string()))
        } else if let Some(title) = line.strip_prefix("### ") {
            Some((chapter == "What Sona does").then(|| capitalized(title.trim())))
        } else {
            None
        };
        match new_heading {
            Some(next) => {
                if let Some(title) = heading.take() {
                    push_parts(&mut sections, &title, &body);
                }
                body.clear();
                heading = next;
            }
            None if heading.is_some() => body.push(line),
            None => {}
        }
    }
    if let Some(title) = heading {
        push_parts(&mut sections, &title, &body);
    }
    sections
}

fn capitalized(title: &str) -> String {
    let mut characters = title.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn push_parts(sections: &mut Vec<HelpSection>, title: &str, body: &[&str]) {
    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in body {
        let line = line.replace("**", "").replace('`', "");
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("- ") {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        }
        if trimmed.is_empty() {
            continue;
        }
        if !current.is_empty() {
            current.push(if trimmed.starts_with("- ") { '\n' } else { ' ' });
        }
        current.push_str(trimmed);
    }
    if !current.is_empty() {
        blocks.push(current);
    }

    let mut parts: Vec<String> = Vec::new();
    let mut part = String::new();
    for block in blocks {
        let block = clipped(&block);
        if !part.is_empty() && part.chars().count() + 1 + block.chars().count() > MAX_TEXT {
            parts.push(std::mem::take(&mut part));
        }
        if !part.is_empty() {
            part.push('\n');
        }
        part.push_str(&block);
    }
    if !part.is_empty() {
        parts.push(part);
    }
    for (index, text) in parts.into_iter().enumerate() {
        sections.push(HelpSection {
            source: "guide".to_string(),
            title: if index == 0 {
                title.to_string()
            } else {
                format!("{title}, part {}", index + 1)
            },
            text,
        });
    }
}

/// One block cut to a section's length at a word boundary, marked with "…".
fn clipped(block: &str) -> String {
    if block.chars().count() <= MAX_TEXT {
        return block.to_string();
    }
    let cut: String = block.chars().take(MAX_TEXT - 1).collect();
    let end = cut.rfind(char::is_whitespace).unwrap_or(cut.len());
    format!("{}…", cut[..end].trim_end())
}

/// One section per group of the sheet's `("Group", [ Entry(title:, keys:) ])`
/// lists, one line per entry: "New task: c".
fn shortcut_sections(sheet: &str) -> Vec<HelpSection> {
    let mut sections = Vec::new();
    let mut group: Option<(String, Vec<String>)> = None;
    for line in sheet.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("(\"") {
            if trimmed.ends_with('[') {
                if let Some((name, _)) = swift_string(rest) {
                    group = Some((name, Vec::new()));
                }
                continue;
            }
        }
        if trimmed.starts_with("])") {
            if let Some((name, entries)) = group.take() {
                sections.push(HelpSection {
                    source: "shortcuts".to_string(),
                    title: format!("Keyboard shortcuts: {name}"),
                    text: clipped(&entries.join("\n")),
                });
            }
            continue;
        }
        if let (Some((_, entries)), Some(entry)) = (group.as_mut(), shortcut_entry(trimmed)) {
            entries.push(entry);
        }
    }
    sections
}

fn shortcut_entry(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Entry(title: \"")?;
    let (title, rest) = swift_string(rest)?;
    let mut keys_list = rest.split_once("keys: [")?.1;
    let mut keys = Vec::new();
    while let Some(start) = keys_list.find('"') {
        let (key, after) = swift_string(&keys_list[start + 1..])?;
        keys.push(key);
        keys_list = after;
        if keys_list.trim_start().starts_with(']') {
            break;
        }
    }
    (!keys.is_empty()).then(|| format!("{title}: {}", keys.join(", ")))
}

/// The Swift string literal body that `text` starts inside, and what follows
/// its closing quote.
fn swift_string(text: &str) -> Option<(String, &str)> {
    let mut value = String::new();
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        match (escaped, character) {
            (true, _) => {
                value.push(character);
                escaped = false;
            }
            (false, '\\') => escaped = true,
            (false, '"') => return Some((value, &text[index + 1..])),
            (false, _) => value.push(character),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::search;

    #[test]
    fn command_menu_shortcut_is_read_from_the_sheet() {
        let found = search("command menu", 20);
        assert!(
            found.iter().any(|section| section.source == "shortcuts"
                && section
                    .text
                    .lines()
                    .any(|line| line == "Command menu: cmd+k")),
            "{found:#?}"
        );
    }

    #[test]
    fn new_task_ranks_its_shortcut_first() {
        let found = search("how do I make a new task", 3);
        let first = found.first().expect("a section answers \"new task\"");
        assert_eq!(first.source, "shortcuts");
        assert!(
            first.text.lines().any(|line| line == "New task: c"),
            "{first:#?}"
        );
    }
}
