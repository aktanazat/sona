//! Shared meeting pages: one encrypted page per meeting on his own site.
//!
//! Nothing here runs on its own. A page goes out only from `publish`, which
//! the share sheet's button and `sona share --confirm` reach; agents can
//! build, save and preview drafts. The page is sealed on the Mac with the
//! shared-link envelope and its key travels only in the link's fragment.

mod audio;
mod content;
mod git;
mod registry;
mod site;
#[cfg(test)]
mod tests;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::cloud_sync::crypto::{
    base64_url_encode, seal_share_payload, SharePayloadContext, SharePayloadDomain,
};
use crate::meeting::analytics::MeetingNotesTemplate;
use crate::meeting::store::{MeetingStore, StoreError};
use crate::meeting::types::MeetingSessionId;
use crate::query::KeptLocal;

pub(crate) use content::{Draft, ExpiresInDays, Include, Page, PageSummary, SpeakerMode};
pub(crate) use registry::{DraftEntry, Paths, ShareRecord, ShareState, SiteSettings};

use content::BuiltPage;
use registry::rfc3339;
use site::Change;

const MAX_TREE_BYTES: u64 = 900 * 1000 * 1000;
const POLL_EVERY: Duration = Duration::from_secs(5);
const POLL_FOR: Duration = Duration::from_secs(4 * 60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const UNTITLED: &str = "Meeting notes";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SharedPagesErrorCode {
    SiteNotSetUp,
    MeetingNotFound,
    TooLarge,
    AudioUnavailable,
    PushFailed,
    NotFound,
    Locked,
    RemoteNotSonaSite,
    InvalidDraft,
    Unavailable,
    Failed,
}

pub(crate) use SharedPagesErrorCode as Code;

#[derive(Clone, Debug)]
pub(crate) struct SharedPagesError {
    pub code: Code,
    pub message: String,
}

impl SharedPagesError {
    pub(crate) fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

type Result<T, E = SharedPagesError> = std::result::Result<T, E>;

/// The meeting a draft is built from, or `None` where meetings cannot be
/// opened; a saved draft never needs one.
pub(crate) type Meetings<'a> = Option<&'a MeetingStore>;

pub(crate) fn meeting_id(value: &str) -> Result<MeetingSessionId> {
    uuid::Uuid::parse_str(value.trim())
        .map(MeetingSessionId::from_uuid)
        .map_err(|_| SharedPagesError::new(Code::MeetingNotFound, "No meeting has that id."))
}

/// The saved draft for a meeting, else one built from the meeting.
pub(crate) fn draft(paths: &Paths, meetings: Meetings<'_>, id: MeetingSessionId) -> Result<Draft> {
    ensure_shareable(meetings, id)?;
    if let Some(draft) = paths.draft::<Draft>(&id.uuid().to_string())? {
        return Ok(draft);
    }
    let store = meetings.ok_or_else(meeting_not_found)?;
    let review = store.review_snapshot(id).map_err(store_error)?;
    let notes = store
        .user_notes(id, MeetingNotesTemplate::default().into())
        .map_err(store_error)?;
    let audio_available = review.session.audio_purged_at_utc_ms.is_none()
        && review
            .tracks
            .iter()
            .any(|track| track.durable_record_count > 0);
    Ok(content::draft_from_meeting(
        &review,
        &notes.body,
        audio_available,
    ))
}

/// Saves `draft`, exporting the recording the first time audio is included.
pub(crate) fn save_draft(paths: &Paths, meetings: Meetings<'_>, draft: &Draft) -> Result<Draft> {
    check_draft(draft)?;
    draft_audio(paths, meetings, draft)?;
    paths.save_draft(&draft.meeting_id, draft)?;
    Ok(draft.clone())
}

pub(crate) fn discard_draft(paths: &Paths, meeting_id: &str) -> Result<()> {
    paths.discard_draft(meeting_id)
}

/// Clears the draft of a page that is already on the site. The page is
/// live either way, so a failure here is logged rather than reported as a
/// failed share that would invite publishing it twice.
pub(crate) fn forget_published_draft(paths: &Paths, meeting_id: &str) {
    if let Err(error) = paths.discard_draft(meeting_id) {
        log::warn!(
            "Shared page is live but its draft could not be cleared: {}",
            error.message
        );
    }
}

pub(crate) fn drafts(paths: &Paths) -> Result<Vec<DraftEntry>> {
    Ok(paths
        .drafts::<Draft>()?
        .into_iter()
        .map(|(draft, saved_at)| DraftEntry {
            title: title_of(&draft),
            meeting_id: draft.meeting_id,
            prepared_by: draft.prepared_by,
            saved_at: rfc3339(saved_at),
        })
        .collect())
}

/// The draft's exported recording when it includes audio, exporting it from
/// the meeting the first time. `None` when audio is off.
pub(crate) fn draft_audio(
    paths: &Paths,
    meetings: Meetings<'_>,
    draft: &Draft,
) -> Result<Option<PathBuf>> {
    if !draft.include.audio {
        return Ok(None);
    }
    let dir = paths.draft_dir(&draft.meeting_id)?;
    let path = audio::draft_audio_path(&dir);
    if path.is_file() {
        return Ok(Some(path));
    }
    let store = meetings
        .filter(|_| draft.audio_available)
        .ok_or_else(audio_unavailable)?;
    let id = meeting_id(&draft.meeting_id)?;
    ensure_shareable(Some(store), id)?;
    let review = store.review_snapshot(id).map_err(|_| audio_unavailable())?;
    fs::create_dir_all(&dir).map_err(registry::io_failed)?;
    audio::export(store, &review, &path)?;
    Ok(Some(path))
}

/// The exact page `draft` would share now. With audio included, `audio`
/// must name the exported recording.
pub(crate) fn build_page(draft: &Draft, audio: Option<&Path>) -> Result<BuiltPageView> {
    check_draft(draft)?;
    let audio = match (draft.include.audio, audio) {
        (false, _) => None,
        (true, Some(path)) => Some(audio::describe(path, draft.content.duration_s)?),
        (true, None) => return Err(audio_unavailable()),
    };
    content::page_from_draft(draft, audio, Utc::now()).map(BuiltPageView)
}

/// A built page, readable but not sealable from outside this module.
pub(crate) struct BuiltPageView(BuiltPage);

impl BuiltPageView {
    pub(crate) fn page(&self) -> &Page {
        &self.0.page
    }

    pub(crate) fn into_page(self) -> Page {
        self.0.page
    }

    pub(crate) fn summary(&self) -> PageSummary {
        PageSummary::of(&self.0)
    }

    /// The share sheet's page, with the recording by file name when
    /// `audio_file_name` is given and the page has audio.
    pub(crate) fn preview_html(&self, audio_file_name: Option<&str>) -> String {
        let json = String::from_utf8_lossy(&self.0.json);
        site::preview_page(
            &json,
            audio_file_name.filter(|_| self.0.page.audio.is_some()),
        )
    }
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct SharedPreview {
    pub page: Page,
    pub html_path: String,
    pub audio_path: Option<String>,
}

/// The share sheet's preview: the page, its preview file in the draft's
/// folder, and the recording beside it.
pub(crate) fn preview(
    paths: &Paths,
    meetings: Meetings<'_>,
    draft: &Draft,
) -> Result<SharedPreview> {
    let audio_path = draft_audio(paths, meetings, draft)?;
    let built = build_page(draft, audio_path.as_deref())?;
    let dir = paths.draft_dir(&draft.meeting_id)?;
    fs::create_dir_all(&dir).map_err(registry::io_failed)?;
    let html_path = dir.join("preview.html");
    let audio_name = audio_path
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    fs::write(&html_path, built.preview_html(audio_name)).map_err(registry::io_failed)?;
    Ok(SharedPreview {
        page: built.into_page(),
        html_path: html_path.display().to_string(),
        audio_path: audio_path.map(|path| path.display().to_string()),
    })
}

/// What a dry run of `share` reports: the page's numbers and the files a
/// push would write, with `<slug>` standing for the slug a real share draws.
#[derive(Clone, Debug, Serialize)]
pub struct DryRun {
    pub page: PageSummary,
    pub files: Vec<String>,
}

pub(crate) fn dry_run(built: &BuiltPageView) -> DryRun {
    let summary = built.summary();
    let mut files = site::site_file_names();
    files.push("<slug>/index.html".to_owned());
    files.push(format!("<slug>/{}", site::PAGE_FILE));
    files.extend((0..summary.audio_parts).map(|index| format!("<slug>/audio.{index}.sona")));
    if built.page().expires_at.is_some() {
        files.push(format!("<slug>/{}", site::EXPIRES_FILE));
    }
    DryRun {
        page: summary,
        files,
    }
}

/// Seals the page and recording, pushes them as a new share, and returns
/// the row in `publishing`. The caller then waits with [`settle`].
pub(crate) fn publish(
    paths: &Paths,
    meetings: Meetings<'_>,
    draft: &Draft,
    audio_path: Option<&Path>,
) -> Result<ShareRecord> {
    check_draft(draft)?;
    ensure_shareable(meetings, meeting_id(&draft.meeting_id)?)?;
    let site = paths.require_site()?;
    let now = Utc::now();
    let audio = match (draft.include.audio, audio_path) {
        (false, _) => None,
        (true, Some(path)) => Some(audio::read(path, draft.content.duration_s)?),
        (true, None) => return Err(audio_unavailable()),
    };
    let built = content::page_from_draft(
        draft,
        audio.as_ref().map(|(page_audio, _)| page_audio.clone()),
        now,
    )?;
    let slug = base64_url_encode(&random::<16>()?);
    let root = random::<32>()?;
    let mut files = vec![(
        site::PAGE_FILE.to_owned(),
        seal(
            &root,
            &slug,
            0,
            1,
            SharePayloadDomain::Manifest,
            &built.json,
        )?,
    )];
    if let Some((page_audio, bytes)) = &audio {
        let total = u64::from(page_audio.parts);
        for (index, part) in (0_u64..).zip(bytes.chunks(audio::PART_BYTES)) {
            files.push((
                format!("audio.{index}.sona"),
                seal(&root, &slug, index, total, SharePayloadDomain::Chunk, part)?,
            ));
        }
    }
    let pushed_bytes = files
        .iter()
        .map(|(_, bytes)| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        .sum();
    if let Some(expires_at) = built.expires_at {
        files.push((
            site::EXPIRES_FILE.to_owned(),
            expires_at.timestamp().to_string().into_bytes(),
        ));
    }
    let record = ShareRecord {
        url: Some(format!(
            "{}/{slug}/#v=1&k={}",
            site.base_url,
            base64_url_encode(&root)
        )),
        meeting_id: draft.meeting_id.clone(),
        title: title_of(draft),
        include: PageSummary::of(&built)
            .sections
            .into_iter()
            .map(str::to_owned)
            .collect(),
        speakers: draft.speakers,
        created_at: rfc3339(now),
        expires_at: built.page.expires_at.clone(),
        state: ShareState::Publishing,
        state_at: rfc3339(now),
        bytes: pushed_bytes,
        error: None,
        slug: slug.clone(),
    };
    let _lock = paths.lock()?;
    let mut shares = paths.shares()?;
    shares.insert(0, record.clone());
    paths.save_shares(&shares)?;
    let pushed = push(
        paths,
        &site,
        &Change::Add {
            slug: &slug,
            files: &files,
        },
    );
    match pushed {
        Ok(()) => Ok(record),
        Err(error) => {
            let failed = update(&mut shares, &slug, |row| {
                row.state = ShareState::Failed;
                row.state_at = rfc3339(Utc::now());
                row.url = None;
                row.error = Some(error.message.clone());
            });
            if failed.is_some() {
                paths.save_shares(&shares)?;
            }
            Err(error)
        }
    }
}

/// Pushes the removal of a share and returns its row in `removing`. A failed
/// publish may still have reached the remote, so it is removed the same way.
pub(crate) fn revoke(paths: &Paths, slug: &str) -> Result<ShareRecord> {
    let _lock = paths.lock()?;
    let mut shares = paths.shares()?;
    let row = shares
        .iter()
        .find(|row| row.slug == slug)
        .cloned()
        .ok_or_else(|| SharedPagesError::new(Code::NotFound, "No share has that link."))?;
    match row.state {
        ShareState::Removed => return Ok(row.seen_at(Utc::now())),
        ShareState::Publishing
        | ShareState::Live
        | ShareState::Failed
        | ShareState::Removing
        | ShareState::Expired => {}
    }
    let site = paths.require_site()?;
    let removing = update(&mut shares, slug, |row| {
        row.state = ShareState::Removing;
        row.state_at = rfc3339(Utc::now());
        row.error = None;
    });
    paths.save_shares(&shares)?;
    if let Err(error) = push(paths, &site, &Change::Remove(slug)) {
        update(&mut shares, slug, |current| {
            current.state = row.state;
            current.state_at = row.state_at.clone();
            current.error = Some(error.message.clone());
        });
        paths.save_shares(&shares)?;
        return Err(error);
    }
    removing.ok_or_else(|| SharedPagesError::new(Code::NotFound, "No share has that link."))
}

fn push(paths: &Paths, site: &SiteSettings, change: &Change<'_>) -> Result<()> {
    git::publish(&paths.clone_dir(), &site.remote, |dir| {
        let written =
            site::write_tree(dir, change, Utc::now().timestamp()).map_err(registry::io_failed)?;
        if written.bytes > MAX_TREE_BYTES {
            return Err(SharedPagesError::new(
                Code::TooLarge,
                "The site would be larger than 900 MB. Remove a share with audio first.",
            ));
        }
        Ok(())
    })
}

/// Waits for a pushed share to reach `target` at the site, checking now and
/// then every 5 seconds for up to 4 minutes. On timeout the row stays
/// pending and the next list checks it again.
pub(crate) async fn settle(paths: &Paths, slug: &str, target: ShareState) -> Result<ShareRecord> {
    let site = paths.require_site()?;
    let client = probe_client()?;
    let started = std::time::Instant::now();
    loop {
        let seen = probe(&client, &site, slug).await;
        if reached(seen, target) {
            let paths = paths.clone();
            let slug = slug.to_owned();
            return tauri::async_runtime::spawn_blocking(move || mark(&paths, &slug, target))
                .await
                .map_err(|_| {
                    SharedPagesError::new(Code::Failed, "The share could not be updated.")
                })?;
        }
        if started.elapsed() + POLL_EVERY > POLL_FOR {
            return current(paths, slug);
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Every share, newest first, as he sees it now. Each pending row is
/// checked once against the site.
pub(crate) async fn list(paths: &Paths) -> Result<Vec<ShareRecord>> {
    let shares = paths.shares()?;
    let mut reached_rows = Vec::new();
    if let Some(site) = paths.site()? {
        let client = probe_client()?;
        for row in &shares {
            let target = match row.state {
                ShareState::Publishing => ShareState::Live,
                ShareState::Removing => ShareState::Removed,
                _ => continue,
            };
            if reached(probe(&client, &site, &row.slug).await, target) {
                reached_rows.push((row.slug.clone(), target));
            }
        }
    }
    let now = Utc::now();
    let forgets_link = shares
        .iter()
        .any(|row| row.url.is_some() && row.state == ShareState::Live && row.has_expired(now));
    if reached_rows.is_empty() && !forgets_link {
        return Ok(shares.into_iter().map(|row| row.seen_at(now)).collect());
    }
    let paths = paths.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _lock = paths.lock()?;
        let mut shares = paths.shares()?;
        for (slug, target) in &reached_rows {
            update(&mut shares, slug, |row| arrive(row, *target));
        }
        let now = Utc::now();
        for row in &mut shares {
            if row.state == ShareState::Live && row.has_expired(now) {
                row.url = None;
            }
        }
        paths.save_shares(&shares)?;
        Ok(shares.into_iter().map(|row| row.seen_at(now)).collect())
    })
    .await
    .map_err(|_| SharedPagesError::new(Code::Failed, "The shares could not be read."))?
}

/// The share a link or slug names.
pub(crate) fn slug_of(link: &str) -> Result<String> {
    let path = link.split('#').next().unwrap_or_default();
    path.split('/')
        .rev()
        .find(|segment| !segment.is_empty())
        .filter(|segment| site::is_slug(segment))
        .map(str::to_owned)
        .ok_or_else(|| SharedPagesError::new(Code::NotFound, "That is not a shared page link."))
}

pub(crate) fn set_site(paths: &Paths, settings: SiteSettings) -> Result<SiteSettings> {
    let remote = settings.remote.trim().to_owned();
    let base_url = settings.base_url.trim().trim_end_matches('/').to_owned();
    if remote.is_empty() || remote.chars().any(char::is_whitespace) || remote.starts_with('-') {
        return Err(SharedPagesError::new(
            Code::InvalidDraft,
            "The remote is not a git URL.",
        ));
    }
    let parsed = reqwest::Url::parse(&base_url)
        .map_err(|_| SharedPagesError::new(Code::InvalidDraft, "The base URL is not a URL."))?;
    if !matches!(parsed.scheme(), "https" | "http")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(SharedPagesError::new(
            Code::InvalidDraft,
            "The base URL must be an http or https address without a query.",
        ));
    }
    let settings = SiteSettings { remote, base_url };
    let _lock = paths.lock()?;
    paths.set_site(&settings)?;
    Ok(settings)
}

pub(crate) fn current(paths: &Paths, slug: &str) -> Result<ShareRecord> {
    paths
        .shares()?
        .into_iter()
        .find(|row| row.slug == slug)
        .map(|row| row.seen_at(Utc::now()))
        .ok_or_else(|| SharedPagesError::new(Code::NotFound, "No share has that link."))
}

fn mark(paths: &Paths, slug: &str, target: ShareState) -> Result<ShareRecord> {
    let _lock = paths.lock()?;
    let mut shares = paths.shares()?;
    let row = update(&mut shares, slug, |row| arrive(row, target))
        .ok_or_else(|| SharedPagesError::new(Code::NotFound, "No share has that link."))?;
    paths.save_shares(&shares)?;
    Ok(row.seen_at(Utc::now()))
}

/// Moves a pending row to what the site showed; a row another write has
/// since moved on is left alone.
fn arrive(row: &mut ShareRecord, target: ShareState) {
    let pending = match target {
        ShareState::Live => ShareState::Publishing,
        ShareState::Removed => ShareState::Removing,
        _ => return,
    };
    if row.state != pending {
        return;
    }
    row.state = target;
    row.state_at = rfc3339(Utc::now());
    if target == ShareState::Removed {
        row.url = None;
    }
}

fn update(
    shares: &mut [ShareRecord],
    slug: &str,
    change: impl FnOnce(&mut ShareRecord),
) -> Option<ShareRecord> {
    let row = shares.iter_mut().find(|row| row.slug == slug)?;
    change(row);
    Some(row.clone())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Seen {
    Present,
    Gone,
    Unknown,
}

fn reached(seen: Seen, target: ShareState) -> bool {
    matches!(
        (seen, target),
        (Seen::Present, ShareState::Live) | (Seen::Gone, ShareState::Removed)
    )
}

fn probe_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|_| SharedPagesError::new(Code::Failed, "The site could not be checked."))
}

async fn probe(client: &reqwest::Client, site: &SiteSettings, slug: &str) -> Seen {
    let url = format!("{}/{slug}/{}", site.base_url, site::PAGE_FILE);
    match client
        .get(url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => Seen::Present,
        Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => Seen::Gone,
        _ => Seen::Unknown,
    }
}

fn check_draft(draft: &Draft) -> Result<()> {
    if draft.version != content::DRAFT_VERSION {
        return Err(SharedPagesError::new(
            Code::InvalidDraft,
            "The draft is not a version 1 draft.",
        ));
    }
    meeting_id(&draft.meeting_id).map(|_| ()).map_err(|_| {
        SharedPagesError::new(
            Code::InvalidDraft,
            "The draft's meeting id is not a meeting id.",
        )
    })
}

fn title_of(draft: &Draft) -> String {
    let title = draft
        .content
        .title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        UNTITLED.to_owned()
    } else {
        title
    }
}

fn seal(
    root: &[u8; 32],
    slug: &str,
    index: u64,
    total: u64,
    domain: SharePayloadDomain,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let context = SharePayloadContext {
        share_id: slug,
        index,
        total,
        domain,
    };
    seal_share_payload(root, &context, &random::<12>()?, plaintext)
        .map_err(|_| SharedPagesError::new(Code::Failed, "The page could not be encrypted."))
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes)
        .map_err(|_| SharedPagesError::new(Code::Failed, "No random bytes were available."))?;
    Ok(bytes)
}

fn store_error(error: StoreError) -> SharedPagesError {
    match error {
        StoreError::NotFound => meeting_not_found(),
        _ => SharedPagesError::new(Code::Failed, "The meeting could not be read."),
    }
}

fn meeting_not_found() -> SharedPagesError {
    SharedPagesError::new(Code::MeetingNotFound, "No meeting has that id.")
}

/// A meeting whose series is kept on this Mac is never shared. Without the
/// store only a portable profile, which holds no meetings, may go on.
pub(crate) fn ensure_shareable(meetings: Meetings<'_>, id: MeetingSessionId) -> Result<()> {
    let Some(store) = meetings else {
        if crate::portable::is_portable() {
            return Ok(());
        }
        return Err(SharedPagesError::new(
            Code::Unavailable,
            "Meetings are not open, so this meeting cannot be checked for sharing.",
        ));
    };
    let kept_local = KeptLocal::read(store)
        .map_err(|_| SharedPagesError::new(Code::Failed, "The meeting could not be read."))?;
    if kept_local.hides(id) {
        return Err(meeting_not_found());
    }
    Ok(())
}

fn audio_unavailable() -> SharedPagesError {
    SharedPagesError::new(
        Code::AudioUnavailable,
        "The recording is not available to share.",
    )
}
