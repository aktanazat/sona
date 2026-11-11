//! Files, images and links on a task. A file is copied into Sona's data
//! folder, `<root>/<task id>/<attachment id>-<safe name>`, so it outlives
//! the original; the same bytes or the same link land on a task once.

use super::super::store::{self, Refusal};
use super::comments::Author;
use super::items;
use super::types::{
    WorkActivityKind, WorkActor, WorkAttachment, WorkAttachmentKind, WorkCommentAuthor,
};
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) const MAX_BYTES: u64 = 50 * 1024 * 1024;
const MAX_NAME_CHARS: usize = 120;
const LINK_SCHEMES: [&str; 4] = ["http", "https", "mailto", "sona"];

const COLUMNS: &str = "id, task_id, kind, title, subtitle, url, stored_name, original_name, mime,
    size, sha256, created_at_ms, created_by_kind, created_by_name";

/// A file's bytes copied into the staging folder and hashed, waiting to be
/// moved under its task. Dropping it removes whatever is still staged.
pub(crate) struct Staged {
    path: PathBuf,
    name: String,
    mime: &'static str,
    size: u64,
    sha256: String,
}

impl Staged {
    pub(crate) fn sha256(&self) -> &str {
        &self.sha256
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The last part of `name` with anything a file system or a shell could
/// read as structure replaced, never empty and never hidden.
pub(crate) fn safe_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned: String = cleaned
        .trim()
        .trim_start_matches('.')
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    if cleaned.trim().is_empty() {
        "file".to_owned()
    } else {
        cleaned.trim().to_owned()
    }
}

fn mime(name: &str) -> &'static str {
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "zip" => "application/zip",
        "mov" => "video/quicktime",
        "mp4" => "video/mp4",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "key" => "application/vnd.apple.keynote",
        "pages" => "application/vnd.apple.pages",
        "numbers" => "application/vnd.apple.numbers",
        _ => "application/octet-stream",
    }
}

/// Copies at most [`MAX_BYTES`] from `source` into a new staging file under
/// `root`, hashing on the way.
fn stage(root: &Path, mut source: impl Read, name: &str) -> Result<Staged> {
    let staging = root.join(".staging");
    fs::create_dir_all(&staging)?;
    let path = staging.join(Uuid::new_v4().to_string());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut staged = Staged {
        path,
        name: name.to_owned(),
        mime: mime(name),
        size: 0,
        sha256: String::new(),
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        staged.size += u64::try_from(read)?;
        if staged.size > MAX_BYTES {
            return Err(Refusal::TooLarge.into());
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
    }
    file.sync_all()?;
    staged.sha256 = hex::encode(hasher.finalize());
    Ok(staged)
}

/// Refuses `source` unless it is a regular file within the size cap.
pub(crate) fn check_file(source: &Path) -> Result<()> {
    let metadata = fs::metadata(source).map_err(|_| Refusal::NoFile)?;
    if !metadata.is_file() {
        return Err(Refusal::NotAFile.into());
    }
    if metadata.len() > MAX_BYTES {
        return Err(Refusal::TooLarge.into());
    }
    Ok(())
}

/// Stages the regular file at `source`, the path as the caller gave it.
pub(crate) fn stage_file(root: &Path, source: &Path) -> Result<Staged> {
    check_file(source)?;
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = File::open(source).map_err(|_| Refusal::NoFile)?;
    stage(root, file, &name)
}

/// Stages pasted bytes under the name they came with.
pub(crate) fn stage_bytes(root: &Path, bytes: &[u8], name: &str) -> Result<Staged> {
    if !u64::try_from(bytes.len()).is_ok_and(|len| len <= MAX_BYTES) {
        return Err(Refusal::TooLarge.into());
    }
    let name = name.rsplit(['/', '\\']).next().unwrap_or_default().trim();
    if name.is_empty() {
        return Err(Refusal::Invalid.into());
    }
    stage(root, bytes, name)
}

fn read(root: &Path) -> impl Fn(&Row<'_>) -> rusqlite::Result<WorkAttachment> + '_ {
    move |row| {
        let item_id: String = row.get(1)?;
        let stored: Option<String> = row.get(6)?;
        Ok(WorkAttachment {
            id: row.get(0)?,
            path: stored.map(|stored| {
                root.join(&item_id)
                    .join(stored)
                    .to_string_lossy()
                    .into_owned()
            }),
            item_id,
            kind: row.get(2)?,
            title: row.get(3)?,
            subtitle: row.get(4)?,
            url: row.get(5)?,
            original_name: row.get(7)?,
            mime: row.get(8)?,
            size: row.get(9)?,
            sha256: row.get(10)?,
            created_at_ms: row.get(11)?,
            created_by: WorkCommentAuthor {
                kind: row.get(12)?,
                name: row.get(13)?,
            },
        })
    }
}

fn find(conn: &Connection, root: &Path, id: &str) -> Result<WorkAttachment> {
    let id = store::uuid(id)?;
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM planning_task_attachments WHERE id = ?1"),
        [&id],
        read(root),
    )
    .optional()?
    .ok_or_else(|| Refusal::NotFound.into())
}

fn existing(
    conn: &Connection,
    root: &Path,
    task_id: &str,
    column: &str,
    value: &str,
) -> Result<Option<WorkAttachment>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM planning_task_attachments WHERE task_id = ?1 AND {column} = ?2"
            ),
            params![task_id, value],
            read(root),
        )
        .optional()?)
}

fn text(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn owns(author: &Author<'_>, attachment: &WorkAttachment) -> bool {
    author.actor == WorkActor::Me
        || (attachment.created_by.kind == author.actor
            && match (author.name(), attachment.created_by.name.as_deref()) {
                (Some(mine), Some(theirs)) => mine.eq_ignore_ascii_case(theirs),
                (None, None) => true,
                _ => false,
            })
}

fn note(
    conn: &Connection,
    task_id: &str,
    author: &Author<'_>,
    from: Option<&str>,
    to: Option<&str>,
    now_ms: i64,
) -> Result<()> {
    let activity_id = items::record(
        conn,
        task_id,
        now_ms,
        author.actor,
        WorkActivityKind::Attachment,
        from,
        to,
    )?;
    if let Some(name) = author.name() {
        conn.execute(
            "INSERT INTO planning_activity_agents (activity_id, agent) VALUES (?1, ?2)",
            params![activity_id, name],
        )?;
    }
    Ok(())
}

/// The title and subtitle a caller gives an attachment; left out, the
/// title is the file's name or the link.
#[derive(Clone, Copy, serde::Serialize)]
pub(crate) struct Naming<'a> {
    pub(crate) title: Option<&'a str>,
    pub(crate) subtitle: Option<&'a str>,
}

struct NewAttachment<'a> {
    kind: WorkAttachmentKind,
    title: &'a str,
    subtitle: Option<&'a str>,
    url: Option<&'a str>,
    stored_name: Option<&'a str>,
    original_name: Option<&'a str>,
    mime: Option<&'a str>,
    size: Option<u64>,
    sha256: Option<&'a str>,
}

fn insert(
    conn: &Connection,
    id: &str,
    task_id: &str,
    new: &NewAttachment<'_>,
    author: &Author<'_>,
    now_ms: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO planning_task_attachments
            (id, task_id, kind, title, subtitle, url, stored_name, original_name, mime, size,
             sha256, created_at_ms, created_by_kind, created_by_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            id,
            task_id,
            new.kind.as_str(),
            new.title,
            new.subtitle,
            new.url,
            new.stored_name,
            new.original_name,
            new.mime,
            new.size,
            new.sha256,
            now_ms,
            author.actor.as_str(),
            author.name()
        ],
    )?;
    note(conn, task_id, author, None, Some(new.title), now_ms)
}

/// Records `staged` on the task as a new attachment, or answers the one
/// already holding the same bytes. [`place`] moves the file in once this
/// commits.
pub(crate) fn attach_file(
    conn: &Connection,
    root: &Path,
    task_id: &str,
    staged: &Staged,
    naming: Naming<'_>,
    author: Author<'_>,
    now_ms: i64,
) -> Result<WorkAttachment> {
    let task_id = items::item(conn, &store::uuid(task_id)?)?.id;
    if let Some(found) = existing(conn, root, &task_id, "sha256", &staged.sha256)? {
        return Ok(found);
    }
    let id = Uuid::new_v4().to_string();
    let stored_name = format!("{id}-{}", safe_name(&staged.name));
    let kind = if staged.mime.starts_with("image/") {
        WorkAttachmentKind::Image
    } else {
        WorkAttachmentKind::File
    };
    insert(
        conn,
        &id,
        &task_id,
        &NewAttachment {
            kind,
            title: text(naming.title).unwrap_or(&staged.name),
            subtitle: text(naming.subtitle),
            url: None,
            stored_name: Some(&stored_name),
            original_name: Some(&staged.name),
            mime: Some(staged.mime),
            size: Some(staged.size),
            sha256: Some(&staged.sha256),
        },
        &author,
        now_ms,
    )?;
    find(conn, root, &id)
}

/// Moves `staged` to where `attachment` keeps its file, unless a file is
/// already there or the attachment is gone.
pub(crate) fn place(conn: &Connection, staged: &Staged, attachment: &WorkAttachment) -> Result<()> {
    let Some(path) = attachment.path.as_deref().map(Path::new) else {
        return Ok(());
    };
    let attached: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM planning_task_attachments WHERE id = ?1)",
        [&attachment.id],
        |row| row.get(0),
    )?;
    if !attached || path.exists() {
        return Ok(());
    }
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    fs::rename(&staged.path, path)?;
    Ok(())
}

/// The link `url` normalised, refused unless it is web, mail or Sona.
pub(crate) fn link(url: &str) -> Result<String> {
    let parsed = url::Url::parse(url.trim()).map_err(|_| Refusal::BadLink)?;
    if !LINK_SCHEMES.contains(&parsed.scheme()) {
        return Err(Refusal::BadLink.into());
    }
    Ok(parsed.into())
}

/// A link on the task under `title`, or the one already there.
pub(crate) fn attach_link(
    conn: &Connection,
    root: &Path,
    task_id: &str,
    url: &str,
    naming: Naming<'_>,
    author: Author<'_>,
    now_ms: i64,
) -> Result<WorkAttachment> {
    let task_id = items::item(conn, &store::uuid(task_id)?)?.id;
    let url = link(url)?;
    if let Some(found) = existing(conn, root, &task_id, "url", &url)? {
        return Ok(found);
    }
    let id = Uuid::new_v4().to_string();
    insert(
        conn,
        &id,
        &task_id,
        &NewAttachment {
            kind: WorkAttachmentKind::Link,
            title: text(naming.title).unwrap_or(&url),
            subtitle: text(naming.subtitle),
            url: Some(&url),
            stored_name: None,
            original_name: None,
            mime: None,
            size: None,
            sha256: None,
        },
        &author,
        now_ms,
    )?;
    find(conn, root, &id)
}

/// A task's attachments, oldest first.
pub(crate) fn list(conn: &Connection, root: &Path, task_id: &str) -> Result<Vec<WorkAttachment>> {
    let task_id = items::item(conn, &store::uuid(task_id)?)?.id;
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM planning_task_attachments WHERE task_id = ?1
         ORDER BY created_at_ms, id"
    ))?;
    let rows = statement.query_map([&task_id], read(root))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// How many attachments each task has, by the task's id.
pub(crate) fn counts(conn: &Connection) -> Result<HashMap<String, u32>> {
    let mut statement =
        conn.prepare("SELECT task_id, count(*) FROM planning_task_attachments GROUP BY task_id")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Takes an attachment off its task, refused as not found when it is on
/// another task than `on_task`. The user may remove any; an agent or Sona
/// only its own. The caller removes the file once this commits.
pub(crate) fn detach(
    conn: &Connection,
    root: &Path,
    id: &str,
    on_task: Option<&str>,
    author: Author<'_>,
    now_ms: i64,
) -> Result<WorkAttachment> {
    let found = find(conn, root, id)?;
    if on_task.is_some_and(|task| task != found.item_id) {
        return Err(Refusal::NotFound.into());
    }
    if !owns(&author, &found) {
        return Err(Refusal::NotAttacher.into());
    }
    conn.execute(
        "DELETE FROM planning_task_attachments WHERE id = ?1",
        [&found.id],
    )?;
    note(
        conn,
        &found.item_id,
        &author,
        Some(&found.title),
        None,
        now_ms,
    )?;
    Ok(found)
}

/// Removes a detached attachment's file, and its task's folder once empty.
pub(crate) fn remove_file(attachment: &WorkAttachment) {
    let Some(path) = attachment.path.as_deref().map(Path::new) else {
        return;
    };
    if let Err(error) = fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!("A detached attachment's file could not be removed: {error}");
        }
    }
    if let Some(folder) = path.parent() {
        let _ = fs::remove_dir(folder);
    }
}

/// An attachment's new title. The user may rename any; an agent or Sona
/// only its own.
pub(crate) fn rename(
    conn: &Connection,
    root: &Path,
    id: &str,
    title: &str,
    author: Author<'_>,
) -> Result<WorkAttachment> {
    let found = find(conn, root, id)?;
    if !owns(&author, &found) {
        return Err(Refusal::NotAttacher.into());
    }
    let title = text(Some(title)).ok_or(Refusal::Invalid)?;
    conn.execute(
        "UPDATE planning_task_attachments SET title = ?2 WHERE id = ?1",
        params![found.id, title],
    )?;
    find(conn, root, &found.id)
}

#[cfg(test)]
mod tests {
    use super::super::super::schema;
    use super::super::comments::Author;
    use super::super::items::{self, NewItem};
    use super::super::types::{WorkActor, WorkOwner, WorkStatus};
    use super::{attach_file, attach_link, detach, list, place, stage_bytes, Naming};
    use rusqlite::Connection;
    use std::path::Path;

    const AT: i64 = 1_700_000_000_000;
    const ME: Author<'static> = Author {
        actor: WorkActor::Me,
        agent: None,
    };
    const UNNAMED: Naming<'static> = Naming {
        title: None,
        subtitle: None,
    };

    fn task(conn: &Connection, title: &str) -> String {
        items::insert(
            conn,
            &NewItem {
                title,
                details: None,
                status: WorkStatus::NotStarted,
                priority: 0,
                project_id: None,
                milestone_id: None,
                owner: &WorkOwner::Nobody,
                due_at_ms: None,
                label_ids: &[],
                source_loop_id: None,
                recurrence: None,
            },
            WorkActor::Me,
            AT,
        )
        .expect("a task")
        .id
    }

    fn tracker() -> (Connection, String) {
        let conn = Connection::open_in_memory().expect("an in-memory history database");
        schema::apply_all(&conn).expect("the planner schema");
        let task = task(&conn, "Launch the harbor site");
        (conn, task)
    }

    #[test]
    fn a_pasted_name_cannot_leave_the_tasks_folder() {
        let (conn, task) = tracker();
        let root = tempfile::tempdir().expect("a scratch folder");
        let staged = stage_bytes(root.path(), b"payload", "../../evil.sh").expect("staged");
        let attached =
            attach_file(&conn, root.path(), &task, &staged, UNNAMED, ME, AT).expect("attached");
        place(&conn, &staged, &attached).expect("placed");
        let path = Path::new(attached.path.as_deref().expect("a stored file"));
        assert_eq!(path.parent(), Some(root.path().join(&task).as_path()));
        assert!(path.is_file());
    }

    #[test]
    fn an_attach_answered_after_a_detach_leaves_no_file() {
        let (conn, task) = tracker();
        let root = tempfile::tempdir().expect("a scratch folder");
        let staged = stage_bytes(root.path(), b"payload", "notes.txt").expect("staged");
        let attached =
            attach_file(&conn, root.path(), &task, &staged, UNNAMED, ME, AT).expect("attached");
        detach(&conn, root.path(), &attached.id, Some(&task), ME, AT + 1).expect("detached");
        place(&conn, &staged, &attached).expect("nothing to place");
        drop(staged);
        assert!(!Path::new(attached.path.as_deref().expect("a stored file")).exists());
    }

    #[test]
    fn the_same_file_twice_is_one_attachment() {
        let (conn, task) = tracker();
        let root = tempfile::tempdir().expect("a scratch folder");
        let first = stage_bytes(root.path(), b"same bytes", "notes.txt").expect("staged");
        let first = attach_file(&conn, root.path(), &task, &first, UNNAMED, ME, AT).expect("one");
        let again = stage_bytes(root.path(), b"same bytes", "copy.txt").expect("staged");
        let again =
            attach_file(&conn, root.path(), &task, &again, UNNAMED, ME, AT + 1).expect("two");
        assert_eq!(first.id, again.id);
        assert_eq!(list(&conn, root.path(), &task).expect("listed").len(), 1);
    }

    #[test]
    fn an_attach_that_rolls_back_leaves_no_file() {
        let (mut conn, task) = tracker();
        let root = tempfile::tempdir().expect("a scratch folder");
        let staged = stage_bytes(root.path(), b"payload", "notes.txt").expect("staged");
        let tx = conn.transaction().expect("a transaction");
        let attached =
            attach_file(&tx, root.path(), &task, &staged, UNNAMED, ME, AT).expect("attached");
        drop(tx);
        drop(staged);
        assert!(!Path::new(attached.path.as_deref().expect("a stored file")).exists());
        assert!(!root.path().join(&task).exists());
    }

    #[test]
    fn a_detach_through_another_task_takes_nothing_off() {
        let (conn, venue) = tracker();
        let copy = task(&conn, "Write the copy");
        let root = tempfile::tempdir().expect("a scratch folder");
        let link = attach_link(
            &conn,
            root.path(),
            &copy,
            "https://example.com/brief",
            UNNAMED,
            ME,
            AT,
        )
        .expect("a link on the copy task");
        let refused =
            detach(&conn, root.path(), &link.id, Some(&venue), ME, AT).expect_err("refused");
        assert_eq!(
            crate::planning::failure(refused),
            crate::planning::failure(super::Refusal::NotFound.into())
        );
        assert_eq!(list(&conn, root.path(), &copy).expect("listed").len(), 1);
    }

    #[test]
    fn only_web_mail_and_sona_links_attach() {
        let (conn, task) = tracker();
        let root = tempfile::tempdir().expect("a scratch folder");
        for url in ["file:///etc/passwd", "javascript:alert(1)"] {
            let refused =
                attach_link(&conn, root.path(), &task, url, UNNAMED, ME, AT).expect_err("refused");
            assert_eq!(
                crate::planning::failure(refused),
                "A link must start with http://, https://, mailto: or sona://.",
                "{url}"
            );
        }
        attach_link(
            &conn,
            root.path(),
            &task,
            "mailto:dana@example.com",
            UNNAMED,
            ME,
            AT,
        )
        .expect("a mail link");
    }
}
