use super::{ContextPolicy, ContextSourceStatus, SourceOutcome};
use ignore::WalkBuilder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

const MAX_FILES: usize = 128;
const MAX_IDENTIFIERS: usize = 256;
const MAX_METADATA_BYTES: usize = 6_000;
const MAX_SOURCE_BYTES: u64 = 32_768;
const SCAN_BUDGET: Duration = Duration::from_millis(150);

static DECLARATION: LazyLock<Regex> = LazyLock::new(|| {
    // PANIC: This pattern is a source constant; an invalid edit is a programming error.
    Regex::new(
        r"(?m)^\s*(?:(?:pub(?:\([^)]*\))?|public|private|protected|internal|open|export|default|async|abstract|final|static|sealed|override|unsafe)\s+)*(?:fn|func|function|def|class|struct|enum|trait|protocol|interface|type|const|let|var)\s+([\p{L}_$][\p{L}\p{N}_$]{1,95})\b",
    )
    .expect("project declaration pattern is valid")
});

/// Names only. Neither source text nor the granted absolute folder path enters
/// the prompt or the history receipt.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize, Type)]
pub struct ProjectContext {
    pub files: Vec<String>,
    pub identifiers: Vec<String>,
    pub truncated: bool,
}

pub(super) fn read(policy: ContextPolicy, root: Option<&Path>) -> SourceOutcome<ProjectContext> {
    if !policy.wants_project() {
        return SourceOutcome::Unavailable(ContextSourceStatus::NotRequested);
    }
    let Some(root) = root else {
        return SourceOutcome::Unavailable(ContextSourceStatus::Disabled);
    };
    if let Err(error) = std::fs::read_dir(root) {
        let status = match error.kind() {
            std::io::ErrorKind::PermissionDenied => ContextSourceStatus::PermissionDenied,
            _ => ContextSourceStatus::Failed,
        };
        return SourceOutcome::Unavailable(status);
    }

    let started = Instant::now();
    let mut context = ProjectContext::default();
    let mut identifiers = BTreeSet::new();
    let mut metadata_bytes = 0;
    let mut walker = WalkBuilder::new(root);
    walker
        .require_git(false)
        // A granted subfolder usually sits inside a repository whose ignore
        // rules live above it. Those files are read to exclude more, never to
        // widen the walk: traversal and source reads stay under `root`.
        .parents(true)
        .git_global(true)
        .hidden(true)
        .follow_links(false)
        .max_depth(Some(12));
    for entry in walker.build() {
        if context.files.len() == MAX_FILES || started.elapsed() >= SCAN_BUDGET {
            context.truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                context.truncated = true;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(relative) = entry.path().strip_prefix(root).ok().and_then(Path::to_str) else {
            context.truncated = true;
            continue;
        };
        if metadata_bytes + relative.len() > MAX_METADATA_BYTES {
            context.truncated = true;
            break;
        }
        metadata_bytes += relative.len();
        context
            .files
            .push(relative.replace(std::path::MAIN_SEPARATOR, "/"));
        if !is_source(entry.path()) || identifiers.len() == MAX_IDENTIFIERS {
            continue;
        }
        match read_source(entry.path()) {
            Ok(source) => {
                for declaration in DECLARATION.captures_iter(&source) {
                    let name = &declaration[1];
                    if identifiers.contains(name) {
                        continue;
                    }
                    if identifiers.len() == MAX_IDENTIFIERS
                        || metadata_bytes + name.len() > MAX_METADATA_BYTES
                    {
                        context.truncated = true;
                        break;
                    }
                    metadata_bytes += name.len();
                    identifiers.insert(name.to_string());
                }
            }
            Err(_) => context.truncated = true,
        }
    }
    if context.files.is_empty() {
        return SourceOutcome::Unavailable(if context.truncated {
            ContextSourceStatus::Failed
        } else {
            ContextSourceStatus::Empty
        });
    }
    context.files.sort_unstable();
    context.identifiers = identifiers.into_iter().collect();
    SourceOutcome::Captured(context)
}

fn is_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some(
            "rs" | "swift"
                | "py"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "mjs"
                | "cjs"
                | "mts"
                | "cts"
                | "java"
                | "kt"
                | "cs"
                | "go"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "hpp"
                | "rb"
                | "php"
        )
    )
}

fn read_source(path: &Path) -> std::io::Result<String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular source file",
        ));
    }
    let mut source = String::new();
    file.take(MAX_SOURCE_BYTES).read_to_string(&mut source)?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_context_contains_only_visible_unignored_names() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(root.path().join(".gitignore"), "private.rs\n").unwrap();
        std::fs::write(root.path().join(".ignore"), "generated.rs\n").unwrap();
        std::fs::write(root.path().join("private.rs"), "pub struct PrivateToken;").unwrap();
        std::fs::write(
            root.path().join("generated.rs"),
            "pub struct GeneratedToken;",
        )
        .unwrap();
        std::fs::write(root.path().join(".hidden.rs"), "pub struct HiddenToken;").unwrap();
        std::fs::write(
            root.path().join("src/ChatStore.rs"),
            "pub struct ChatStore;\npub fn load_context() {}\n// not provider input",
        )
        .unwrap();
        let captured = read(ContextPolicy::Full, Some(root.path()));
        assert_eq!(
            captured,
            SourceOutcome::Captured(ProjectContext {
                files: vec!["src/ChatStore.rs".to_string()],
                identifiers: vec!["ChatStore".to_string(), "load_context".to_string()],
                truncated: false,
            })
        );
    }

    #[test]
    fn a_granted_subfolder_keeps_the_ignore_rules_above_it() {
        let repository = tempfile::tempdir().unwrap();
        std::fs::write(repository.path().join(".gitignore"), "src/private.rs\n").unwrap();
        let root = repository.path().join("src");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("private.rs"), "pub struct PrivateToken;").unwrap();
        std::fs::write(root.join("public.rs"), "pub struct PublicToken;").unwrap();
        assert_eq!(
            read(ContextPolicy::Full, Some(&root)),
            SourceOutcome::Captured(ProjectContext {
                files: vec!["public.rs".to_string()],
                identifiers: vec!["PublicToken".to_string()],
                truncated: false,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn linked_files_and_folders_cannot_supply_project_names() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(
            outside.path().join("private.rs"),
            "pub struct OutsideToken;",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("private.rs"),
            root.path().join("link.rs"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked-folder")).unwrap();
        assert_eq!(
            read(ContextPolicy::Full, Some(root.path())),
            SourceOutcome::Unavailable(ContextSourceStatus::Empty)
        );
    }

    #[test]
    fn a_large_project_cannot_expand_the_prompt_without_bound() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..MAX_FILES + 1 {
            std::fs::write(root.path().join(format!("file-{index}.txt")), "").unwrap();
        }
        let SourceOutcome::Captured(context) = read(ContextPolicy::Full, Some(root.path())) else {
            panic!("the directory has readable project files");
        };
        assert!(context.files.len() <= MAX_FILES);
        assert!(context.truncated);
    }
}
