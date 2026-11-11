use std::fs;

use chrono::{TimeZone, Utc};

use super::content::{
    page_from_draft, ActionItem, Draft, DraftContent, Include, SpeakerMode, Turn, DRAFT_VERSION,
};
use super::site::{write_tree, Change, EXPIRES_FILE, MARKER, PAGE_FILE};
use super::{draft, publish, Code, Paths};
use crate::meeting::detection::machine::{
    CalendarAttendee, CalendarEventSummary, ParticipationStatus,
};
use crate::meeting::series_types::MeetingSeriesRemoteOptOutSetRequest;
use crate::meeting::store::workflow_core_tests::{reviewable_meeting, store};
use crate::meeting::types::{MeetingOperationId, MeetingSessionId};

const WHEN: i64 = 1_772_640_000_000;

fn invented_draft() -> Draft {
    Draft {
        version: DRAFT_VERSION,
        meeting_id: "6f1c2a4e-0000-4000-8000-000000000001".to_owned(),
        prepared_by: None,
        include: Include {
            title: true,
            summary: true,
            action_items: true,
            decisions: false,
            notes: false,
            transcript: true,
            audio: false,
        },
        speakers: SpeakerMode::Numbered,
        expires_in_days: None,
        content: DraftContent {
            title: "Greenhouse sensor review".to_owned(),
            date: Some("2026-03-04T15:00:00Z".to_owned()),
            duration_s: Some(1800),
            summary: vec!["The humidity sensors drift after two weeks.".to_owned()],
            decisions: vec!["Switch suppliers before the spring order.".to_owned()],
            action_items: vec![ActionItem {
                text: "Recalibrate bench three.".to_owned(),
                owner: None,
                due: None,
            }],
            notes: vec!["Ask about the warranty quietly.".to_owned()],
            speakers: vec!["Odile Brask".to_owned(), "Teodor Vance".to_owned()],
            transcript: vec![
                Turn {
                    s: 1,
                    t: 0.0,
                    text: "Bench three reads high again.".to_owned(),
                },
                Turn {
                    s: 0,
                    t: 4.5,
                    text: "Then we recalibrate it this week.".to_owned(),
                },
            ],
        },
        audio_available: false,
    }
}

/// A numbered page, given a draft whose speakers have names and whose notes
/// and decisions are turned off, carries none of those names or words.
#[test]
fn numbered_page_leaves_out_speaker_names_and_excluded_sections() {
    let shared_at = Utc.with_ymd_and_hms(2026, 3, 5, 9, 0, 0).unwrap();
    let built = page_from_draft(&invented_draft(), None, shared_at).unwrap();
    let json = String::from_utf8(built.json).unwrap();

    let leaked: Vec<&str> = [
        "Odile Brask",
        "Teodor Vance",
        "Ask about the warranty quietly.",
        "Switch suppliers before the spring order.",
    ]
    .into_iter()
    .filter(|private| json.contains(private))
    .collect();

    assert_eq!(leaked, Vec::<&str>::new(), "page: {json}");
    assert_eq!(
        built.page.speakers,
        Some(vec!["Speaker 1".to_owned(), "Speaker 2".to_owned()])
    );
}

/// Rebuilding the site for a new share, given one share whose `expires` time
/// has passed and one still live, removes the expired one and keeps the live.
#[test]
fn site_rebuild_drops_expired_shares() {
    let root = tempfile::tempdir().unwrap();
    let now = 1_800_000_000_i64;
    let expired = "expiredExpiredExpired0";
    let live = "liveLiveLiveLiveLive00";
    for (slug, expires) in [(expired, now - 1), (live, now + 3_600)] {
        let share = root.path().join(slug);
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join(PAGE_FILE), b"sealed").unwrap();
        fs::write(share.join(EXPIRES_FILE), expires.to_string()).unwrap();
    }
    let files = [(PAGE_FILE.to_owned(), b"sealed".to_vec())];

    write_tree(
        root.path(),
        &Change::Add {
            slug: "newNewNewNewNewNewNew0",
            files: &files,
        },
        now,
    )
    .unwrap();

    assert!(!root.path().join(expired).exists());
    assert!(root.path().join(live).join(PAGE_FILE).is_file());
    assert!(root.path().join(MARKER).is_file());
}

/// The deploy workflow's build step, run on a tree holding a share whose slug
/// starts with `-` (base64url slugs can), publishes that share.
#[test]
fn deploy_build_keeps_slug_starting_with_dash() {
    let root = tempfile::tempdir().unwrap();
    let slug = "-IKmJ-WLVs_MccgjGriYog";
    let files = [(PAGE_FILE.to_owned(), b"sealed".to_vec())];
    write_tree(root.path(), &Change::Add { slug, files: &files }, WHEN / 1000).unwrap();

    let workflow = fs::read_to_string(root.path().join(".github/workflows/pages.yml")).unwrap();
    let indent = "          ";
    let script: String = workflow
        .lines()
        .skip_while(|line| !line.trim_end().ends_with("run: |"))
        .skip(1)
        .take_while(|line| line.is_empty() || line.starts_with(indent))
        .map(|line| format!("{}\n", line.strip_prefix(indent).unwrap_or(line)))
        .collect();
    let status = std::process::Command::new("bash")
        .args(["-e", "-c", &script])
        .current_dir(root.path())
        .status()
        .unwrap();

    assert!(status.success());
    assert!(root.path().join("_site").join(slug).join(PAGE_FILE).is_file());
}

fn weekly(series_key: &str) -> CalendarEventSummary {
    CalendarEventSummary {
        event_key: format!("{series_key}#weekly"),
        series_key: series_key.to_owned(),
        title: "Weekly".to_owned(),
        attendee_count: 2,
        start_utc_ms: WHEN,
        end_utc_ms: WHEN + 1_800_000,
        attendees: vec![CalendarAttendee {
            name: "Mirela Osk".to_owned(),
            status: ParticipationStatus::Accepted,
            email: Some("mirela@example.com".to_owned()),
            is_self: false,
        }],
        notes: None,
        calendar_name: None,
        location: None,
        url: None,
        place: None,
    }
}

/// A meeting whose series is kept on this Mac, handed to `publish` as a
/// draft file would be, is refused as not found before any site is read,
/// while a meeting from another series gets as far as the missing site.
#[test]
fn kept_local_meeting_is_never_shared() {
    let (_directory, store) = store();
    let data = tempfile::tempdir().unwrap();
    let paths = Paths::new(data.path());
    let kept = reviewable_meeting(&store, "Board prep", WHEN);
    let open = reviewable_meeting(&store, "Garden rota", WHEN);
    store
        .remember_calendar_facts(kept, &weekly("board-prep"))
        .unwrap();
    store
        .remember_calendar_facts(open, &weekly("garden-rota"))
        .unwrap();
    store
        .set_series_remote_opt_out(
            &MeetingSeriesRemoteOptOutSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: "board-prep".to_owned(),
                remote_intelligence_opt_out: true,
                expected_revision: store.series_revision().unwrap(),
            },
            WHEN,
        )
        .unwrap();
    let draft_for = |id: MeetingSessionId| Draft {
        meeting_id: id.uuid().to_string(),
        ..invented_draft()
    };

    let kept_publish = publish(&paths, Some(&store), &draft_for(kept), None).unwrap_err();
    let kept_draft = draft(&paths, Some(&store), kept).unwrap_err();
    let open_publish = publish(&paths, Some(&store), &draft_for(open), None).unwrap_err();

    assert_eq!(kept_publish.code, Code::MeetingNotFound);
    assert_eq!(kept_draft.code, Code::MeetingNotFound);
    assert_eq!(open_publish.code, Code::SiteNotSetUp);
}
