use std::fs;

use chrono::{TimeZone, Utc};

use super::checks::{check_page, ShareCheckKind, ShareCheckSection};
use super::content::{
    page_from_draft, ActionItem, Draft, DraftContent, Include, SpeakerMode, Turn, DRAFT_VERSION,
};
use super::site::{write_tree, Change, EXPIRES_FILE, MARKER, PAGE_FILE};
use super::{blurb_of, draft, publish, Code, Paths};
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
        for_person: None,
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

/// A one-person page names that person and keeps only her action items; the
/// name matches however it is spaced or cased, and a page for a person
/// with no items, or with action items left off, is refused.
#[test]
fn person_page_keeps_only_the_persons_items() {
    let mut draft = invented_draft();
    draft.content.action_items = vec![
        ActionItem {
            text: "Recalibrate bench three.".to_owned(),
            owner: Some("  jane   doe ".to_owned()),
            due: None,
        },
        ActionItem {
            text: "Book the cold room.".to_owned(),
            owner: Some("Teodor Vance".to_owned()),
            due: None,
        },
        ActionItem {
            text: "File the drift report.".to_owned(),
            owner: None,
            due: None,
        },
    ];
    let shared_at = Utc.with_ymd_and_hms(2026, 3, 5, 9, 0, 0).unwrap();

    draft.for_person = Some("JANE  DOE".to_owned());
    let page = page_from_draft(&draft, None, shared_at).unwrap().page;
    assert_eq!(page.for_person.as_deref(), Some("JANE DOE"));
    let items = page.action_items.as_deref().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].text, "Recalibrate bench three.");

    for (person, items_on) in [("Nobody Here", true), ("Teodor Vance", false)] {
        draft.for_person = Some(person.to_owned());
        draft.include.action_items = items_on;
        let Err(error) = page_from_draft(&draft, None, shared_at) else {
            panic!("a page for {person} with none of their items is refused");
        };
        assert_eq!(error.code, Code::NoItemsForPerson);
    }
}

/// The checks flag a phone number, an address, an amount, a long digit run
/// and a person who was not in the meeting, each once and exactly as it
/// reads — but not a date, a date beside a time, a year, a version, a
/// speaker or the person the page is for.
#[test]
fn checks_flag_only_what_should_be_replaced() {
    let mut draft = invented_draft();
    draft.include.decisions = true;
    draft.include.notes = true;
    draft.for_person = Some("Marta Illen".to_owned());
    draft.content.title = "Greenhouse sensor review on 2026-11-10".to_owned();
    draft.content.summary = vec![
        "Call Odile Brask on +1 (415) 555-0132 about the drift. The review also covered Marta Illen, Al Reiss and Ted.".to_owned(),
    ];
    draft.content.decisions =
        vec!["Pay $4,500 to mirela@example.com before 10:00 on Tuesday.".to_owned()];
    draft.content.action_items = vec![ActionItem {
        text: "Follow up on ticket 9912034 by Odile Brask.".to_owned(),
        owner: Some("Marta Illen".to_owned()),
        due: Some("2026-11-12".to_owned()),
    }];
    draft.content.notes = vec![
        "The invoice was 2026 dollars for firmware 1.2.3.".to_owned(),
        "Next check-in 2026-11-24 10:30 at the shed.".to_owned(),
        "Pay $420, then call +1 (415) 555-0132 again.".to_owned(),
    ];
    draft.content.transcript = vec![
        Turn {
            s: 0,
            t: 0.0,
            text: "Order placed in 2026, total $850.".to_owned(),
        },
        Turn {
            s: 1,
            t: 4.5,
            text: "The link is https://example.com/spec.".to_owned(),
        },
    ];
    let people = [
        vec!["Marta Illen".to_owned()],
        vec!["Al Reiss".to_owned()],
        vec!["Al".to_owned()],
        vec!["Odile Brask".to_owned()],
        vec!["Ted".to_owned(), "Teodor Vance".to_owned()],
    ];
    let shared_at = Utc.with_ymd_and_hms(2026, 3, 5, 9, 0, 0).unwrap();
    let page = page_from_draft(&draft, None, shared_at).unwrap().page;

    let flags: Vec<(ShareCheckKind, ShareCheckSection, String)> = check_page(
        &page,
        &draft.content.speakers,
        &people,
    )
    .into_iter()
    .map(|check| (check.kind, check.section, check.text))
    .collect();

    let flagged = |kind: ShareCheckKind, section: ShareCheckSection, text: &str| {
        flags
            .iter()
            .any(|(k, s, t)| *k == kind && *s == section && t == text)
    };
    assert!(flagged(ShareCheckKind::Phone, ShareCheckSection::Summary, "+1 (415) 555-0132"));
    assert!(flagged(ShareCheckKind::Email, ShareCheckSection::Decisions, "mirela@example.com"));
    assert!(flagged(ShareCheckKind::Money, ShareCheckSection::Decisions, "$4,500"));
    assert!(flagged(ShareCheckKind::Number, ShareCheckSection::ActionItems, "9912034"));
    assert!(flagged(ShareCheckKind::Link, ShareCheckSection::Transcript, "https://example.com/spec"));
    assert!(flagged(ShareCheckKind::Money, ShareCheckSection::Transcript, "$850"));
    assert!(flagged(ShareCheckKind::Money, ShareCheckSection::Notes, "$420"));
    assert!(flagged(ShareCheckKind::Person, ShareCheckSection::Summary, "Al Reiss"));
    assert_eq!(
        flags.iter().filter(|(_, _, found)| found == "+1 (415) 555-0132").count(),
        1,
        "a repeated number is one flag: {flags:?}"
    );
    assert!(
        !flags.iter().any(|(_, _, found)| {
            found.contains("2026-") || found.contains("10:30") || found == "Marta Illen"
        }),
        "no date, time or the page's own person is flagged: {flags:?}"
    );

    for text in ["2026-11-10", "2026-11-12", "10:00", "2026", "1.2.3", "Al", "Ted"] {
        assert!(
            !flags.iter().any(|(_, _, found)| found.as_str() == text),
            "{text} should not be flagged: {flags:?}"
        );
    }
    assert!(
        !flags
            .iter()
            .any(|(kind, _, found)| *kind == ShareCheckKind::Person
                && (found == "Odile Brask" || found == "Teodor Vance")),
        "a speaker is never flagged: {flags:?}"
    );
}

/// The share's blurb is the first sentence of the first summary paragraph
/// on the page, capped at 200 characters, and absent without a summary.
#[test]
fn blurb_is_the_first_sentence_within_two_hundred_chars() {
    let shared_at = Utc.with_ymd_and_hms(2026, 3, 5, 9, 0, 0).unwrap();
    let mut draft = invented_draft();
    draft.content.summary = vec![
        "The sensors cost $4.50 each. More detail follows.".to_owned(),
        "Second paragraph.".to_owned(),
    ];
    let page = page_from_draft(&draft, None, shared_at).unwrap().page;
    assert_eq!(blurb_of(&page).as_deref(), Some("The sensors cost $4.50 each."));

    draft.content.summary = vec!["x".repeat(300)];
    let page = page_from_draft(&draft, None, shared_at).unwrap().page;
    assert_eq!(
        blurb_of(&page).map(|blurb| blurb.chars().count()),
        Some(200)
    );

    draft.include.summary = false;
    let page = page_from_draft(&draft, None, shared_at).unwrap().page;
    assert_eq!(blurb_of(&page), None);
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
        "https://example.test",
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
    write_tree(
        root.path(),
        "https://example.test",
        &Change::Add { slug, files: &files },
        WHEN / 1000,
    )
    .unwrap();

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
