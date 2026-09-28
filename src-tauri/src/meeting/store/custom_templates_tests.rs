use super::series_tests::calendar_facts;
use super::workflow_core_tests::{reviewable_meeting, store};
use super::*;
use crate::meeting::folder_types::{
    MeetingFolderCreateRequest, MeetingFolderDefaultsSetRequest, MeetingFolderMembershipRequest,
};
use crate::meeting::series_types::{
    MeetingSeriesDigestSetRequest, MeetingSeriesRemoteOptOutSetRequest,
    MeetingSeriesTemplateSetRequest,
};
use crate::meeting::template_types::{
    MeetingCustomTemplateDeleteRequest, MeetingCustomTemplateDraft,
    MeetingCustomTemplateSaveRequest, MeetingCustomTemplateSaveResult, MeetingTemplateSection,
    MAX_CUSTOM_TEMPLATES,
};
use crate::secrets::SecretManager;
use tempfile::TempDir;

fn draft(name: &str) -> MeetingCustomTemplateDraft {
    MeetingCustomTemplateDraft {
        name: name.to_string(),
        purpose: "  Pricing decisions  ".to_string(),
        sections: vec![MeetingTemplateSection {
            title: "  Decisions  ".to_string(),
            instructions: "  State the approved price.  ".to_string(),
        }],
    }
}

fn create(store: &MeetingStore, name: &str) -> MeetingCustomTemplateSaveResult {
    store
        .save_custom_template(
            &MeetingCustomTemplateSaveRequest {
                template_id: None,
                draft: draft(name),
                expected_revision: store.custom_templates().unwrap().revision,
            },
            100,
        )
        .unwrap()
}

fn delete(store: &MeetingStore, template_id: MeetingTemplateId) {
    store
        .delete_custom_template(&MeetingCustomTemplateDeleteRequest {
            template_id,
            expected_revision: store.custom_templates().unwrap().revision,
        })
        .unwrap();
}

fn choose_series(
    store: &MeetingStore,
    template: Option<MeetingNotesTemplate>,
    custom_template_id: Option<MeetingTemplateId>,
) {
    store
        .set_series_template(
            &MeetingSeriesTemplateSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: "weekly".to_string(),
                template,
                custom_template_id,
                expected_revision: store.series_revision().unwrap(),
            },
            200,
        )
        .unwrap();
}

#[test]
fn custom_templates_save_sorted_trimmed_updates_and_fence_every_write() {
    let (_directory, store) = store();
    let first = create(&store, "  zebra ");
    create(&store, "Beta");
    let third = create(&store, "alpha");
    assert_eq!(
        third
            .templates
            .templates
            .iter()
            .map(|template| template.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "Beta", "zebra"]
    );
    let initial = store.custom_template(first.template_id).unwrap().unwrap();
    assert_eq!(initial.purpose, "Pricing decisions");
    assert_eq!(initial.sections[0].title, "Decisions");
    assert_eq!(
        initial.sections[0].instructions,
        "State the approved price."
    );
    let request = MeetingCustomTemplateSaveRequest {
        template_id: Some(first.template_id),
        draft: draft("Updated"),
        expected_revision: 3,
    };
    let updated = store.save_custom_template(&request, 300).unwrap();
    assert_eq!(updated.template_id, first.template_id);
    let template = store.custom_template(first.template_id).unwrap().unwrap();
    assert_eq!(template.name, "Updated");
    assert_eq!(template.created_at_utc_ms, 100);
    assert_eq!(template.updated_at_utc_ms, 300);
    assert_eq!(updated.templates.revision, 4);
    assert_eq!(
        store.save_custom_template(&request, 400),
        Err(StoreError::StaleRevision)
    );
    assert_eq!(
        store.delete_custom_template(&MeetingCustomTemplateDeleteRequest {
            template_id: first.template_id,
            expected_revision: 3,
        }),
        Err(StoreError::StaleRevision)
    );
    assert_eq!(store.custom_templates().unwrap(), updated.templates);
    let deleted = store
        .delete_custom_template(&MeetingCustomTemplateDeleteRequest {
            template_id: first.template_id,
            expected_revision: 4,
        })
        .unwrap();
    assert_eq!(deleted.revision, 5);
    assert_eq!(store.custom_template(first.template_id).unwrap(), None);
}

#[test]
fn custom_templates_reject_invalid_and_missing_without_mutating_the_list() {
    let (_directory, store) = store();
    let before = store.custom_templates().unwrap();
    let missing = MeetingTemplateId::new();
    assert_eq!(
        store.save_custom_template(
            &MeetingCustomTemplateSaveRequest {
                template_id: Some(missing),
                draft: draft("Missing"),
                expected_revision: 0,
            },
            100
        ),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        store.delete_custom_template(&MeetingCustomTemplateDeleteRequest {
            template_id: missing,
            expected_revision: 0,
        }),
        Err(StoreError::NotFound)
    );
    let mut invalid = draft("Weekly");
    invalid.sections.push(invalid.sections[0].clone());
    assert_eq!(
        store.save_custom_template(
            &MeetingCustomTemplateSaveRequest {
                template_id: None,
                draft: invalid,
                expected_revision: 0,
            },
            100
        ),
        Err(StoreError::Invalid)
    );
    assert_eq!(store.custom_templates().unwrap(), before);
}

#[test]
fn custom_templates_limit_allows_updates_but_refuses_the_fifty_first() {
    let (_directory, store) = store();
    let first = create(&store, "First").template_id;
    for index in 1..MAX_CUSTOM_TEMPLATES {
        create(&store, &format!("Template {index}"));
    }
    let full = store.custom_templates().unwrap();
    assert_eq!(full.templates.len(), MAX_CUSTOM_TEMPLATES);
    assert_eq!(
        store.save_custom_template(
            &MeetingCustomTemplateSaveRequest {
                template_id: None,
                draft: draft("Overflow"),
                expected_revision: full.revision,
            },
            200
        ),
        Err(StoreError::Invalid)
    );
    assert_eq!(store.custom_templates().unwrap(), full);
    store
        .save_custom_template(
            &MeetingCustomTemplateSaveRequest {
                template_id: Some(first),
                draft: draft("Renamed"),
                expected_revision: full.revision,
            },
            300,
        )
        .unwrap();
    assert_eq!(
        store.custom_template(first).unwrap().unwrap().name,
        "Renamed"
    );
}

#[test]
fn custom_templates_deletion_preserves_other_series_choices_and_invalidates_stale_writes() {
    let (_directory, store) = store();
    let template_id = create(&store, "Weekly").template_id;
    choose_series(
        &store,
        Some(MeetingNotesTemplate::Interview),
        Some(template_id),
    );
    let chosen = store.series_preferences("weekly").unwrap();
    assert_eq!(chosen.template, None);
    assert_eq!(chosen.custom_template_id, Some(template_id));
    store
        .set_series_digest(
            &MeetingSeriesDigestSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: "weekly".to_string(),
                digest_included: false,
                expected_revision: chosen.revision,
            },
            300,
        )
        .unwrap();
    store
        .set_series_remote_opt_out(
            &MeetingSeriesRemoteOptOutSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: "weekly".to_string(),
                remote_intelligence_opt_out: true,
                expected_revision: store.series_revision().unwrap(),
            },
            400,
        )
        .unwrap();
    let before = store.series_preferences("weekly").unwrap();
    delete(&store, template_id);
    let cleared = store.series_preferences("weekly").unwrap();
    assert_eq!(cleared.custom_template_id, None);
    assert_eq!(cleared.template, None);
    assert!(!cleared.digest_included);
    assert!(cleared.remote_intelligence_opt_out);
    assert_eq!(cleared.revision, before.revision + 1);
    let stale = store
        .set_series_template(
            &MeetingSeriesTemplateSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: "weekly".to_string(),
                template: Some(MeetingNotesTemplate::Standup),
                custom_template_id: None,
                expected_revision: before.revision,
            },
            500,
        )
        .unwrap();
    assert_eq!(stale.receipt.result, OperationResult::Rejected);
    assert_eq!(
        stale.receipt.reason_codes,
        vec![MeetingReasonCode::StaleRevision]
    );
    assert_eq!(stale.preferences, cleared);
}

#[test]
fn custom_templates_unknown_notes_and_series_choices_leave_saved_choices_untouched() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Weekly", 1);
    let saved = store
        .save_user_notes(
            session_id,
            "Keep this",
            MeetingNotesTemplate::Standup,
            None,
            0,
        )
        .unwrap();
    let missing = MeetingTemplateId::new();
    assert_eq!(
        store.save_user_notes(
            session_id,
            "Replace this",
            MeetingNotesTemplate::General,
            Some(missing),
            1
        ),
        Err(StoreError::NotFound)
    );
    assert_eq!(
        store
            .user_notes(session_id, MeetingNotesTemplate::General.into())
            .unwrap(),
        saved
    );
    assert_eq!(
        store.set_series_template(
            &MeetingSeriesTemplateSetRequest {
                operation_id: MeetingOperationId::new(),
                series_key: "weekly".to_string(),
                template: None,
                custom_template_id: Some(missing),
                expected_revision: 0,
            },
            100
        ),
        Err(StoreError::NotFound)
    );
    assert_eq!(store.series_revision().unwrap(), 0);
}

#[test]
fn custom_templates_deleted_choices_fall_through_meeting_series_folder_and_settings() {
    let (_directory, store) = store();
    let session_id = reviewable_meeting(&store, "Weekly", 1);
    calendar_facts(&store, session_id, "weekly", "Weekly");
    let app_template = create(&store, "App").template_id;
    let series_template = create(&store, "Series").template_id;
    let own_template = create(&store, "Own").template_id;
    let default = NotesTemplateChoice {
        template: MeetingNotesTemplate::Interview,
        custom_template_id: Some(app_template),
    };
    assert_eq!(
        store.notes_template_fallback(session_id, default).unwrap(),
        default
    );
    let created = store
        .create_meeting_folder(
            &MeetingFolderCreateRequest {
                operation_id: MeetingOperationId::new(),
                name: "Clients".to_string(),
                expected_revision: 0,
            },
            100,
        )
        .unwrap();
    let folder_id = created.folders.folders[0].folder_id;
    let configured = store
        .set_meeting_folder_defaults(
            &MeetingFolderDefaultsSetRequest {
                operation_id: MeetingOperationId::new(),
                folder_id,
                template: Some(MeetingNotesTemplate::Standup),
                prompt_ids: vec![],
                expected_revision: created.folders.revision,
            },
            200,
        )
        .unwrap();
    store
        .add_meeting_to_folder(
            &MeetingFolderMembershipRequest {
                operation_id: MeetingOperationId::new(),
                folder_id,
                session_id,
                expected_revision: configured.folders.revision,
            },
            300,
        )
        .unwrap();
    assert_eq!(
        store.notes_template_fallback(session_id, default).unwrap(),
        MeetingNotesTemplate::Standup.into()
    );
    choose_series(&store, Some(MeetingNotesTemplate::SalesCall), None);
    assert_eq!(
        store.notes_template_fallback(session_id, default).unwrap(),
        MeetingNotesTemplate::SalesCall.into()
    );
    choose_series(&store, None, Some(series_template));
    let own = store
        .save_user_notes(
            session_id,
            "Keep these notes",
            MeetingNotesTemplate::General,
            Some(own_template),
            0,
        )
        .unwrap();
    let effective = || {
        let fallback = store.notes_template_fallback(session_id, default).unwrap();
        store.user_notes(session_id, fallback).unwrap()
    };
    assert_eq!(effective().custom_template_id, Some(own_template));
    let evidence = store
        .artifact_evidence(
            session_id,
            1024,
            store.notes_template_fallback(session_id, default).unwrap(),
        )
        .unwrap();
    assert_eq!(
        evidence.template.artifact_template_id(),
        format!("custom:{}", own_template.uuid())
    );
    delete(&store, own_template);
    assert_eq!(effective().custom_template_id, Some(series_template));
    assert_eq!(effective().body, own.body);
    assert_eq!(effective().revision, own.revision);
    delete(&store, series_template);
    assert_eq!(effective().custom_template_id, None);
    assert_eq!(effective().template, MeetingNotesTemplate::Standup);
    store
        .set_meeting_folder_defaults(
            &MeetingFolderDefaultsSetRequest {
                operation_id: MeetingOperationId::new(),
                folder_id,
                template: None,
                prompt_ids: vec![],
                expected_revision: store.meeting_folder_list().unwrap().revision,
            },
            400,
        )
        .unwrap();
    assert_eq!(effective().custom_template_id, Some(app_template));
    delete(&store, app_template);
    assert_eq!(effective().custom_template_id, None);
    assert_eq!(effective().template, MeetingNotesTemplate::Interview);
    assert_eq!(effective().body, "Keep these notes");
}

#[test]
fn custom_templates_migration_preserves_existing_notes_and_series_preferences() {
    let migrations = Migrations::new(MIGRATIONS.to_vec());
    let mut probe = Connection::open_in_memory().unwrap();
    let version = (1..=MIGRATIONS.len()).find(|&version| {
        migrations.to_version(&mut probe, version).unwrap();
        probe.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'meeting_custom_templates')",
            [], |row| row.get::<_, bool>(0)).unwrap()
    }).unwrap();
    let directory = TempDir::new().unwrap();
    let secrets = SecretManager::with_backend(Arc::new(crate::secrets::MemorySecretBackend::new()));
    let root = directory.path().join("meetings");
    fs::create_dir_all(&root).unwrap();
    let key = tauri::async_runtime::block_on(secrets.meeting_storage_key()).unwrap();
    let mut connection = open_encrypted_connection(&root.join("meeting-store.db"), &key).unwrap();
    migrations.to_version(&mut connection, version - 1).unwrap();
    let session_id = MeetingSessionId::new();
    connection
        .execute(
            "INSERT INTO meeting_sessions (id, phase, revision, title, origin_kind, preflight_json,
            created_at_utc_ms, started_at_utc_ms, processing_status, retention_policy_json)
         VALUES (?1, 'review_ready', 0, 'Weekly', 'manual', '{}', 1000, 1000,
            '{\"kind\":\"succeeded\"}', '{\"kind\":\"forever\"}')",
            params![id(session_id)],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO meeting_user_notes (session_id, body, template_id, note_revision, updated_at_utc_ms)
         VALUES (?1, 'Existing notes', 'meeting-interview', 7, 1000)", params![id(session_id)],
    ).unwrap();
    connection.execute(
        "INSERT INTO meeting_series_preferences (series_key, template_id, digest_included, updated_at_utc_ms)
         VALUES ('weekly', 'meeting-standup', 0, 1000)", [],
    ).unwrap();
    drop(connection);
    let key = tauri::async_runtime::block_on(secrets.meeting_storage_key()).unwrap();
    let store = MeetingStore::open(root, key).unwrap();
    let notes = store
        .user_notes(session_id, MeetingNotesTemplate::General.into())
        .unwrap();
    assert_eq!(notes.body, "Existing notes");
    assert_eq!(notes.template, MeetingNotesTemplate::Interview);
    assert_eq!(notes.custom_template_id, None);
    assert_eq!(notes.revision, 7);
    let series = store.series_preferences("weekly").unwrap();
    assert_eq!(series.template, Some(MeetingNotesTemplate::Standup));
    assert_eq!(series.custom_template_id, None);
    assert!(!series.digest_included);
    let template_id = create(&store, "After upgrade").template_id;
    store
        .save_user_notes(
            session_id,
            &notes.body,
            notes.template,
            Some(template_id),
            notes.revision,
        )
        .unwrap();
    choose_series(&store, None, Some(template_id));
    assert_eq!(
        store
            .user_notes(session_id, MeetingNotesTemplate::General.into())
            .unwrap()
            .custom_template_id,
        Some(template_id)
    );
    assert_eq!(
        store
            .series_preferences("weekly")
            .unwrap()
            .custom_template_id,
        Some(template_id)
    );
}
