//! The tree Sona writes into the shared pages repository, and the two ways
//! `share-site/page.html` is filled: published (assets by content-hashed
//! name) and preview (everything inline, for the share sheet).

use std::fs;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

const PAGE_HTML: &str = include_str!("../../../share-site/page.html");
const VIEWER_JS: &str = include_str!("../../../share-site/viewer.js");
const VIEWER_CSS: &str = include_str!("../../../share-site/viewer.css");
const INDEX_HTML: &str = include_str!("../../../share-site/index.html");
const NOT_FOUND_HTML: &str = include_str!("../../../share-site/404.html");
const PAGES_YML: &str = include_str!("../../../share-site/pages.yml");

/// The root file that marks a repository as one Sona writes. A remote whose
/// `main` lacks it is someone else's, and is never pushed over.
pub(crate) const MARKER: &str = ".sona-site";
const MARKER_TEXT: &str = "sona shared pages v1\n";
const SLUG_LEN: usize = 22;
pub(crate) const EXPIRES_FILE: &str = "expires";
pub(crate) const PAGE_FILE: &str = "page.sona";

const PUBLISHED_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; media-src blob:; img-src 'self'; base-uri 'none'; form-action 'none'";
const PREVIEW_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; media-src file: blob:; img-src data:";

pub(crate) fn is_slug(name: &str) -> bool {
    name.len() == SLUG_LEN
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn asset_name(stem: &str, extension: &str, contents: &str) -> String {
    let digest = hex::encode(Sha256::digest(contents.as_bytes()));
    format!("{stem}.{}.{extension}", &digest[..12])
}

fn fill(head: &str, boot: &str) -> String {
    PAGE_HTML
        .replacen("{{HEAD}}", head, 1)
        .replacen("{{BOOT}}", boot, 1)
}

/// A slug's `index.html`.
pub(crate) fn published_page() -> String {
    let css = asset_name("viewer", "css", VIEWER_CSS);
    let js = asset_name("viewer", "js", VIEWER_JS);
    fill(
        &format!(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"{PUBLISHED_CSP}\">\n<link rel=\"stylesheet\" href=\"../assets/{css}\">"
        ),
        &format!("<script type=\"module\" src=\"../assets/{js}\"></script>"),
    )
}

/// The share sheet's page: the page JSON, the viewer and its style inline,
/// and the local audio file by name when there is one.
pub(crate) fn preview_page(page_json: &str, audio_file_name: Option<&str>) -> String {
    let mut boot = format!(
        "<script type=\"application/json\" id=\"sona-page\">{}</script>\n",
        page_json.replace('<', "\\u003c")
    );
    if let Some(name) = audio_file_name {
        let name = serde_json::to_string(name).unwrap_or_default();
        boot.push_str(&format!(
            "<script type=\"application/json\" id=\"sona-audio\">{}</script>\n",
            name.replace('<', "\\u003c")
        ));
    }
    boot.push_str(&format!(
        "<script type=\"module\">{}</script>",
        VIEWER_JS.replace("</script", "<\\/script")
    ));
    fill(
        &format!(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"{PREVIEW_CSP}\">\n<style>{}</style>",
            VIEWER_CSS.replace("</style", "<\\/style")
        ),
        &boot,
    )
}

/// The one change a write makes to the tree.
pub(crate) enum Change<'a> {
    Add {
        slug: &'a str,
        files: &'a [(String, Vec<u8>)],
    },
    Remove(&'a str),
}

pub(crate) struct Written {
    pub bytes: u64,
}

/// Rewrites `root` (a working tree; `.git` is left alone) as the whole tree
/// Sona publishes: fresh site files, every unexpired share kept with its
/// page pointing at the current assets, expired ones dropped, then `change`.
pub(crate) fn write_tree(root: &Path, change: &Change<'_>, now_unix: i64) -> io::Result<Written> {
    let (Change::Remove(touched) | Change::Add { slug: touched, .. }) = change;
    let mut kept = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        let is_share = entry.file_type()?.is_dir() && is_slug(&name);
        if is_share && name != *touched && !expired(&path, now_unix) {
            kept.push(path);
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    let assets = root.join("assets");
    fs::create_dir_all(&assets)?;
    fs::create_dir_all(root.join(".github/workflows"))?;
    fs::write(root.join(".github/workflows/pages.yml"), PAGES_YML)?;
    fs::write(root.join("index.html"), INDEX_HTML)?;
    fs::write(root.join("404.html"), NOT_FOUND_HTML)?;
    fs::write(root.join(MARKER), MARKER_TEXT)?;
    fs::write(
        assets.join(asset_name("viewer", "js", VIEWER_JS)),
        VIEWER_JS,
    )?;
    fs::write(
        assets.join(asset_name("viewer", "css", VIEWER_CSS)),
        VIEWER_CSS,
    )?;
    let page = published_page();
    for share in &kept {
        fs::write(share.join("index.html"), &page)?;
    }
    if let Change::Add { slug, files } = change {
        let share = root.join(slug);
        fs::create_dir_all(&share)?;
        fs::write(share.join("index.html"), &page)?;
        for (name, bytes) in *files {
            fs::write(share.join(name), bytes)?;
        }
    }
    Ok(Written {
        bytes: tree_bytes(root)?,
    })
}

/// The files one share adds beside its `index.html`, as `write_tree` would
/// write them, for a dry run.
pub(crate) fn site_file_names() -> Vec<String> {
    vec![
        MARKER.to_owned(),
        ".github/workflows/pages.yml".to_owned(),
        "index.html".to_owned(),
        "404.html".to_owned(),
        format!("assets/{}", asset_name("viewer", "js", VIEWER_JS)),
        format!("assets/{}", asset_name("viewer", "css", VIEWER_CSS)),
    ]
}

fn expired(share: &Path, now_unix: i64) -> bool {
    fs::read_to_string(share.join(EXPIRES_FILE))
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok())
        .is_some_and(|expires| expires <= now_unix)
}

fn tree_bytes(path: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let kind = entry.file_type()?;
        total += if kind.is_dir() {
            tree_bytes(&entry.path())?
        } else {
            entry.metadata()?.len()
        };
    }
    Ok(total)
}
