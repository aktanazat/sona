//! `sona share-preview`, `share`, `shares`, `unshare`, `share-site`: shared
//! meeting pages from a script or an agent.
//!
//! Agents can prepare and preview; a page goes out only from `share
//! --confirm` run at a terminal where he then types `share`, or his click in
//! Sona. Meetings kept local are never shared, from a draft file either.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, ValueEnum};
use serde::Serialize;

use super::{store, ExternalError, ExternalErrorCode, ExternalScope, READ_SCOPES, WRITE_SCOPES};
use crate::meeting::session::MeetingSessionManager;
use crate::meeting::store::MeetingStore;
use crate::shared_pages::{
    self, Code, Draft, DryRun, ExpiresInDays, Include, Page, PageSummary, Paths, ShareRecord,
    ShareState, SharedPagesError, SiteSettings, SpeakerMode,
};

/// What goes on the page, beside the draft's own choices.
#[derive(Args, Clone, Debug, Default, Eq, PartialEq)]
pub struct ShareChoices {
    /// Exactly these sections: title, summary, action_items, decisions,
    /// notes, transcript, audio. Comma-separated.
    #[arg(long, value_name = "SECTIONS", value_delimiter = ',')]
    pub include: Option<Vec<String>>,
    /// Speakers by name, or as Speaker 1, Speaker 2.
    #[arg(long, value_enum)]
    pub speakers: Option<SpeakerChoice>,
    /// When the page stops working: 1d, 7d, 30d or never.
    #[arg(long, value_name = "WHEN")]
    pub expires: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SpeakerChoice {
    Names,
    Numbered,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SharePreviewArgs {
    /// The meeting's id.
    #[arg(value_name = "MEETING")]
    pub meeting: String,
    #[command(flatten)]
    pub choices: ShareChoices,
    /// Keep this draft in Sona, to check and share from there.
    #[arg(long)]
    pub save: bool,
    /// Who prepared the draft, shown on the share sheet.
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
    /// Write the page as a standalone HTML file.
    #[arg(long, value_name = "FILE")]
    pub html: Option<PathBuf>,
    /// Print one JSON value instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ShareArgs {
    /// The meeting's id; leave out with --draft.
    #[arg(value_name = "MEETING", required_unless_present = "draft")]
    pub meeting: Option<String>,
    /// Share a draft file; audio comes from audio.m4a beside it.
    #[arg(long, value_name = "FILE")]
    pub draft: Option<PathBuf>,
    #[command(flatten)]
    pub choices: ShareChoices,
    /// Publish, after `share` is typed back at the terminal.
    #[arg(long, conflicts_with = "dry_run")]
    pub confirm: bool,
    /// Show what would be published, and publish nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Print one JSON value instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct SharesArgs {
    /// Print one JSON value instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct UnshareArgs {
    /// The share's slug or link.
    #[arg(value_name = "SLUG_OR_URL")]
    pub link: String,
    /// Show which share would be removed, and remove nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Print one JSON value instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Clone, Debug, Eq, PartialEq)]
pub struct ShareSiteArgs {
    /// The git remote shares are pushed to.
    #[arg(long, value_name = "URL", requires = "base_url")]
    pub remote: Option<String>,
    /// The address the site serves that repository at.
    #[arg(long, value_name = "URL", requires = "remote")]
    pub base_url: Option<String>,
    /// Print one JSON value instead of text.
    #[arg(long)]
    pub json: bool,
}

/// One checked share verb.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShareVerb {
    Preview(SharePreviewArgs),
    Share(ShareArgs),
    Shares,
    Unshare(UnshareArgs),
    Site(ShareSiteArgs),
}

impl ShareVerb {
    pub(super) const fn required_scopes(&self) -> &'static [ExternalScope] {
        match self {
            Self::Preview(args) if args.save => WRITE_SCOPES,
            Self::Share(args) if args.confirm => WRITE_SCOPES,
            Self::Unshare(args) if !args.dry_run => WRITE_SCOPES,
            Self::Site(args) if args.remote.is_some() => WRITE_SCOPES,
            Self::Preview(_) | Self::Share(_) | Self::Shares | Self::Unshare(_) | Self::Site(_) => {
                READ_SCOPES
            }
        }
    }

    /// The checks clap cannot make, the publish gate first: a share is
    /// either a dry run or confirmed at a terminal.
    pub(super) fn checked(self) -> Result<Self, ExternalError> {
        let choices = match &self {
            Self::Preview(args) => &args.choices,
            Self::Share(args) => {
                if !args.confirm && !args.dry_run {
                    return Err(ExternalError::new(
                        ExternalErrorCode::ConfirmationRequired,
                        "Sharing publishes this meeting's page. Add --dry-run to see it, or --confirm to publish.",
                    ));
                }
                if args.confirm && !std::io::stdin().is_terminal() {
                    return Err(ExternalError::new(
                        ExternalErrorCode::ConfirmationRequiresTerminal,
                        "--confirm publishes only when typed at a terminal. Share from Sona instead.",
                    ));
                }
                &args.choices
            }
            Self::Shares | Self::Unshare(_) | Self::Site(_) => return Ok(self),
        };
        parse_choices(choices)?;
        Ok(self)
    }
}

#[derive(Clone, Debug, Default)]
struct Choices {
    include: Option<Include>,
    speakers: Option<SpeakerMode>,
    expires: Option<Option<ExpiresInDays>>,
}

impl Choices {
    fn apply(&self, draft: &mut Draft) {
        if let Some(include) = self.include {
            draft.include = include;
        }
        if let Some(speakers) = self.speakers {
            draft.speakers = speakers;
        }
        if let Some(expires) = self.expires {
            draft.expires_in_days = expires;
        }
    }
}

fn parse_choices(choices: &ShareChoices) -> Result<Choices, ExternalError> {
    let include = choices
        .include
        .as_ref()
        .map(|names| Include::from_names(names.iter().map(String::as_str)))
        .transpose()
        .map_err(ExternalError::invalid)?;
    let expires = choices
        .expires
        .as_deref()
        .map(|value| match value.trim() {
            "never" => Ok(None),
            "1d" => ExpiresInDays::try_from(1).map(Some),
            "7d" => ExpiresInDays::try_from(7).map(Some),
            "30d" => ExpiresInDays::try_from(30).map(Some),
            _ => Err("--expires is 1d, 7d, 30d or never."),
        })
        .transpose()
        .map_err(ExternalError::invalid)?;
    Ok(Choices {
        include,
        speakers: choices.speakers.map(|choice| match choice {
            SpeakerChoice::Names => SpeakerMode::Names,
            SpeakerChoice::Numbered => SpeakerMode::Numbered,
        }),
        expires,
    })
}

/// The page a preview shows, and what became of the draft.
#[derive(Clone, Debug, Serialize)]
pub struct SharePreviewAnswer {
    pub meeting_id: String,
    pub page: Page,
    pub summary: PageSummary,
    pub saved: bool,
    pub prepared_by: Option<String>,
    /// Audio is included but not yet exported; saving or sharing exports it.
    pub audio_pending: bool,
    pub html_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ShareDryRunAnswer {
    pub dry_run: bool,
    pub meeting_id: String,
    pub audio_pending: bool,
    #[serde(flatten)]
    pub plan: DryRun,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnshareDryRunAnswer {
    pub dry_run: bool,
    pub share: ShareRecord,
    pub would_remove: Vec<String>,
}

/// One share answer. Serialised untagged, like the corpus answers.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ShareAnswer {
    Preview(Box<SharePreviewAnswer>),
    DryRun(Box<ShareDryRunAnswer>),
    Share(ShareRecord),
    Shares(Vec<ShareRecord>),
    UnshareDryRun(Box<UnshareDryRunAnswer>),
    Site(Option<SiteSettings>),
}

pub(super) async fn answer(
    verb: &ShareVerb,
    app: &tauri::AppHandle,
    meetings: &Arc<MeetingSessionManager>,
) -> Result<ShareAnswer, ExternalError> {
    let paths = crate::portable::app_data_dir(app)
        .map(|dir| Paths::new(&dir))
        .map_err(|_| {
            ExternalError::new(
                ExternalErrorCode::Failed,
                "Sona's data folder could not be found.",
            )
        })?;
    match verb {
        ShareVerb::Preview(args) => preview(&paths, meetings, args).await,
        ShareVerb::Share(args) => share(&paths, meetings, args).await,
        ShareVerb::Shares => Ok(ShareAnswer::Shares(
            shared_pages::list(&paths).await.map_err(refusal)?,
        )),
        ShareVerb::Unshare(args) => unshare(&paths, args).await,
        ShareVerb::Site(args) => match (&args.remote, &args.base_url) {
            (Some(remote), Some(base_url)) => shared_pages::set_site(
                &paths,
                SiteSettings {
                    remote: remote.clone(),
                    base_url: base_url.clone(),
                },
            )
            .map(|site| ShareAnswer::Site(Some(site)))
            .map_err(refusal),
            _ => paths.site().map(ShareAnswer::Site).map_err(refusal),
        },
    }
}

/// The meeting's draft through this surface: the store must be open, and
/// `shared_pages::draft` refuses meetings kept local.
async fn meeting_draft(
    paths: &Paths,
    meetings: &Arc<MeetingSessionManager>,
    meeting: &str,
) -> Result<(Draft, Arc<MeetingStore>), ExternalError> {
    let id = shared_pages::meeting_id(meeting).map_err(refusal)?;
    let store = store(meetings).await?;
    let draft = shared_pages::draft(paths, Some(&store), id).map_err(refusal)?;
    Ok((draft, store))
}

/// The draft's recording when it is already exported; nothing is written.
fn exported_audio(paths: &Paths, draft: &Draft) -> Option<PathBuf> {
    let path = paths.draft_dir(&draft.meeting_id).ok()?.join("audio.m4a");
    (draft.include.audio && path.is_file()).then_some(path)
}

/// A page built without exporting anything: audio that is not exported yet
/// is left off and reported pending.
fn page_now(
    draft: &Draft,
    audio: Option<&Path>,
) -> Result<(shared_pages::BuiltPageView, bool), ExternalError> {
    let pending = draft.include.audio && audio.is_none();
    let mut shown = draft.clone();
    shown.include.audio &= !pending;
    let built = shared_pages::build_page(&shown, audio).map_err(refusal)?;
    Ok((built, pending))
}

async fn preview(
    paths: &Paths,
    meetings: &Arc<MeetingSessionManager>,
    args: &SharePreviewArgs,
) -> Result<ShareAnswer, ExternalError> {
    let choices = parse_choices(&args.choices)?;
    let (mut draft, store) = meeting_draft(paths, meetings, &args.meeting).await?;
    choices.apply(&mut draft);
    let audio = if args.save {
        draft.prepared_by = args
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        shared_pages::save_draft(paths, Some(&store), &draft).map_err(refusal)?;
        shared_pages::draft_audio(paths, Some(&store), &draft).map_err(refusal)?
    } else {
        exported_audio(paths, &draft)
    };
    let (built, audio_pending) = page_now(&draft, audio.as_deref())?;
    let html_path = match &args.html {
        Some(path) => {
            std::fs::write(path, built.preview_html(None)).map_err(|error| {
                ExternalError::new(
                    ExternalErrorCode::Failed,
                    format!("{} could not be written: {error}", path.display()),
                )
            })?;
            Some(path.clone())
        }
        None => None,
    };
    Ok(ShareAnswer::Preview(Box::new(SharePreviewAnswer {
        meeting_id: draft.meeting_id.clone(),
        summary: built.summary(),
        page: built.into_page(),
        saved: args.save,
        prepared_by: draft.prepared_by,
        audio_pending,
        html_path,
    })))
}

async fn share(
    paths: &Paths,
    meetings: &Arc<MeetingSessionManager>,
    args: &ShareArgs,
) -> Result<ShareAnswer, ExternalError> {
    let choices = parse_choices(&args.choices)?;
    let (mut draft, store, file_audio) = match &args.draft {
        Some(file) => {
            let draft = read_draft(file)?;
            if args
                .meeting
                .as_deref()
                .is_some_and(|meeting| meeting.trim() != draft.meeting_id)
            {
                return Err(ExternalError::invalid(
                    "The draft file is for a different meeting.",
                ));
            }
            let audio = file.parent().unwrap_or(Path::new(".")).join("audio.m4a");
            let store = if args.dry_run {
                None
            } else {
                meetings.store().await.ok()
            };
            (draft, store, Some(audio))
        }
        None => {
            let meeting = args.meeting.as_deref().unwrap_or_default();
            let (draft, store) = meeting_draft(paths, meetings, meeting).await?;
            (draft, Some(store), None)
        }
    };
    choices.apply(&mut draft);
    let audio = match &file_audio {
        Some(path) => Some(path.clone()).filter(|path| draft.include.audio && path.is_file()),
        None if args.dry_run => exported_audio(paths, &draft),
        None => shared_pages::draft_audio(paths, store.as_deref(), &draft).map_err(refusal)?,
    };
    if args.dry_run {
        let (built, audio_pending) = if file_audio.is_some() {
            (
                shared_pages::build_page(&draft, audio.as_deref()).map_err(refusal)?,
                false,
            )
        } else {
            page_now(&draft, audio.as_deref())?
        };
        return Ok(ShareAnswer::DryRun(Box::new(ShareDryRunAnswer {
            dry_run: true,
            meeting_id: draft.meeting_id.clone(),
            audio_pending,
            plan: shared_pages::dry_run(&built),
        })));
    }
    let id = shared_pages::meeting_id(&draft.meeting_id).map_err(refusal)?;
    shared_pages::ensure_shareable(store.as_deref(), id).map_err(refusal)?;
    let site = paths.require_site().map_err(refusal)?;
    let page = shared_pages::build_page(&draft, audio.as_deref())
        .map_err(refusal)?
        .into_page();
    typed_confirmation(&draft, &page, &site)?;
    let record = shared_pages::publish(paths, store.as_deref(), &draft, audio.as_deref())
        .map_err(refusal)?;
    if file_audio.is_none() {
        shared_pages::discard_draft(paths, &draft.meeting_id).map_err(refusal)?;
    }
    shared_pages::settle(paths, &record.slug, ShareState::Live)
        .await
        .map(ShareAnswer::Share)
        .map_err(refusal)
}

/// The human step behind `--confirm`: the page's outline goes to the
/// terminal itself, and only `share` typed back there publishes it.
fn typed_confirmation(
    draft: &Draft,
    page: &Page,
    site: &SiteSettings,
) -> Result<(), ExternalError> {
    let refused = || {
        ExternalError::new(
            ExternalErrorCode::ConfirmationRequired,
            "Nothing was shared: the page goes out only after share is typed at the terminal.",
        )
    };
    let sections = [
        ("title", page.title.is_some()),
        ("summary", page.summary.is_some()),
        ("action items", page.action_items.is_some()),
        ("decisions", page.decisions.is_some()),
        ("notes", page.notes.is_some()),
        ("transcript", page.transcript.is_some()),
        ("audio", page.audio.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect::<Vec<_>>()
    .join(", ");
    let title = page
        .title
        .as_deref()
        .unwrap_or_else(|| draft.content.title.trim());
    let prompt = format!(
        "Share \"{title}\"\n  sections: {sections}\n  expires: {}\n  site: {}\nType share and press Return to publish: ",
        page.expires_at.as_deref().unwrap_or("never"),
        site.base_url
    );
    let mut terminal = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| refused())?;
    terminal
        .write_all(prompt.as_bytes())
        .and_then(|()| terminal.flush())
        .map_err(|_| refused())?;
    let mut answer = String::new();
    BufReader::new(&terminal)
        .read_line(&mut answer)
        .map_err(|_| refused())?;
    if answer.trim() == "share" {
        Ok(())
    } else {
        Err(refused())
    }
}

async fn unshare(paths: &Paths, args: &UnshareArgs) -> Result<ShareAnswer, ExternalError> {
    let slug = shared_pages::slug_of(&args.link).map_err(refusal)?;
    if args.dry_run {
        let share = shared_pages::current(paths, &slug).map_err(refusal)?;
        return Ok(ShareAnswer::UnshareDryRun(Box::new(UnshareDryRunAnswer {
            dry_run: true,
            would_remove: if share.state == ShareState::Removed {
                Vec::new()
            } else {
                vec![format!("{slug}/")]
            },
            share,
        })));
    }
    let removing = shared_pages::revoke(paths, &slug).map_err(refusal)?;
    if removing.state != ShareState::Removing {
        return Ok(ShareAnswer::Share(removing));
    }
    shared_pages::settle(paths, &slug, ShareState::Removed)
        .await
        .map(ShareAnswer::Share)
        .map_err(refusal)
}

fn read_draft(path: &Path) -> Result<Draft, ExternalError> {
    let bytes = std::fs::read(path).map_err(|error| {
        ExternalError::invalid(format!("{} could not be read: {error}", path.display()))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        ExternalError::invalid(format!("{} is not a share draft: {error}", path.display()))
    })
}

fn refusal(error: SharedPagesError) -> ExternalError {
    let code = match error.code {
        Code::SiteNotSetUp => ExternalErrorCode::SiteNotSetUp,
        Code::MeetingNotFound | Code::NotFound => ExternalErrorCode::NotFound,
        Code::Unavailable => ExternalErrorCode::Unavailable,
        Code::TooLarge => ExternalErrorCode::TooLarge,
        Code::AudioUnavailable => ExternalErrorCode::AudioUnavailable,
        Code::PushFailed => ExternalErrorCode::PushFailed,
        Code::Locked => ExternalErrorCode::Locked,
        Code::RemoteNotSonaSite => ExternalErrorCode::RemoteNotSonaSite,
        Code::InvalidDraft => ExternalErrorCode::InvalidRequest,
        Code::Failed => ExternalErrorCode::Failed,
    };
    ExternalError::new(code, error.message)
}

impl ShareAnswer {
    pub(super) fn text(&self) -> String {
        let mut text = String::new();
        match self {
            Self::Preview(preview) => {
                let _ = writeln!(
                    text,
                    "{}",
                    preview.page.title.as_deref().unwrap_or("Meeting notes")
                );
                write_summary(&mut text, &preview.summary);
                if preview.audio_pending {
                    text.push_str("audio: exported when the draft is saved or shared\n");
                }
                if preview.saved {
                    text.push_str("draft saved; share it from Sona\n");
                }
                if let Some(path) = &preview.html_path {
                    let _ = writeln!(text, "html: {}", path.display());
                }
            }
            Self::DryRun(plan) => {
                text.push_str("dry run: nothing was published\n");
                write_summary(&mut text, &plan.plan.page);
                if plan.audio_pending {
                    text.push_str("audio: exported when shared\n");
                }
                text.push_str("files:\n");
                for file in &plan.plan.files {
                    let _ = writeln!(text, "  {file}");
                }
            }
            Self::Share(record) => write_record(&mut text, record),
            Self::Shares(records) => {
                if records.is_empty() {
                    text.push_str("No shared pages.\n");
                }
                for record in records {
                    write_record(&mut text, record);
                    text.push('\n');
                }
            }
            Self::UnshareDryRun(plan) => {
                text.push_str("dry run: nothing was removed\n");
                write_record(&mut text, &plan.share);
                for path in &plan.would_remove {
                    let _ = writeln!(text, "would remove: {path}");
                }
            }
            Self::Site(Some(site)) => {
                let _ = writeln!(text, "remote: {}\nbase url: {}", site.remote, site.base_url);
            }
            Self::Site(None) => text.push_str("No site is set up for shared pages.\n"),
        }
        text
    }
}

fn write_summary(text: &mut String, summary: &PageSummary) {
    let _ = writeln!(text, "sections: {}", summary.sections.join(", "));
    let _ = writeln!(
        text,
        "summary {}, decisions {}, action items {}, notes {}, speakers {}, turns {}",
        summary.summary_paragraphs,
        summary.decisions,
        summary.action_items,
        summary.notes_paragraphs,
        summary.speakers,
        summary.transcript_turns
    );
    let _ = writeln!(
        text,
        "page bytes {}, audio parts {}, audio bytes {}",
        summary.bytes, summary.audio_parts, summary.audio_bytes
    );
}

fn write_record(text: &mut String, record: &ShareRecord) {
    let state = serde_json::to_value(record.state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    let _ = writeln!(text, "{state}  {}  {}", record.title, record.slug);
    if let Some(url) = &record.url {
        let _ = writeln!(text, "{url}");
    }
    let _ = writeln!(
        text,
        "shared {}; expires {}; contains {}",
        record.created_at,
        record.expires_at.as_deref().unwrap_or("never"),
        record.include.join(", ")
    );
    if let Some(error) = &record.error {
        let _ = writeln!(text, "error: {error}");
    }
}

#[cfg(test)]
mod tests {
    use crate::cli::CliArgs;
    use crate::query::external::{ExternalErrorCode, ExternalRequest};
    use clap::Parser;

    /// `sona share`, given neither --confirm nor --dry-run, is refused with
    /// confirmation_required (exit 2) before any consent or store is read.
    #[test]
    fn share_without_confirm_is_refused() {
        let args = CliArgs::try_parse_from([
            "sona",
            "share",
            "--draft",
            "/private/var/tmp/no-such-draft.json",
        ])
        .unwrap();

        let refusal = ExternalRequest::from_args(&args).unwrap_err();

        assert_eq!(refusal.error, ExternalErrorCode::ConfirmationRequired);
        assert_eq!(refusal.error.exit_code(), 2);
    }
}
