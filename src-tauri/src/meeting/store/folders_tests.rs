//! Folders at the store boundary: the migration that adds them, and what a
//! folder may and may not do to the meetings filed in it.

use super::workflow_core_tests::{reviewable_meeting, store};
use super::*;
use crate::meeting::folder_types::{
    MeetingFolderCreateRequest, MeetingFolderDeleteRequest, MeetingFolderMembershipRequest,
    MeetingFolderMutationResult,
};
use crate::secrets::SecretManager;
use tempfile::TempDir;

fn create(store: &MeetingStore, name: &str, revision: u64) -> MeetingFolderMutationResult {
    // PANIC: a fixture; a setup that fails should fail the test that asked.
    store
        .create_meeting_folder(
            &MeetingFolderCreateRequest {
                operation_id: MeetingOperationId::new(),
                name: name.to_string(),
                expected_revision: revision,
            },
            1,
        )
        .unwrap()
}

fn folder_named(result: &MeetingFolderMutationResult, name: &str) -> MeetingFolderId {
    // PANIC: a fixture; a folder the test just made must be listed.
    result
        .folders
        .folders
        .iter()
        .find(|folder| folder.name == name)
        .map(|folder| folder.folder_id)
        .expect("the folder is listed")
}

fn file(
    store: &MeetingStore,
    folder_id: MeetingFolderId,
    session_id: MeetingSessionId,
    revision: u64,
) -> MeetingFolderMutationResult {
    // PANIC: a fixture; a setup that fails should fail the test that asked.
    store
        .add_meeting_to_folder(
            &MeetingFolderMembershipRequest {
                operation_id: MeetingOperationId::new(),
                folder_id,
                session_id,
                expected_revision: revision,
            },
            2,
        )
        .unwrap()
}

fn listed(store: &MeetingStore, folder_id: Option<MeetingFolderId>) -> Vec<MeetingSessionId> {
    store
        .list_sessions(
            None,
            100,
            &MeetingListFilter {
                folder_id,
                ..MeetingListFilter::default()
            },
        )
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.session_id)
        .collect()
}

fn has_table(connection: &Connection, name: &str) -> bool {
    // PANIC: a fixture; the schema query cannot fail on an open connection.
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
            params![name],
            |row| row.get(0),
        )
        .unwrap()
}

/// A store written by the build before folders existed opens under this one,
/// reads its meetings back, and can file them.
#[test]
fn a_store_written_before_folders_opens_and_files_its_meetings() {
    let migrations = Migrations::new(MIGRATIONS.to_vec());
    // The first version with folders, found rather than counted, so a
    // migration appended later cannot quietly move this test's starting point.
    let mut probe = Connection::open_in_memory().unwrap();
    let with_folders = (1..=MIGRATIONS.len())
        .find(|&version| {
            migrations.to_version(&mut probe, version).unwrap();
            has_table(&probe, "meeting_folders")
        })
        .expect("a migration creates the folders");

    let directory = TempDir::new().unwrap();
    let secrets = SecretManager::with_backend(Arc::new(crate::secrets::MemorySecretBackend::new()));
    let root = directory.path().join("meetings");
    fs::create_dir_all(&root).unwrap();
    let key = tauri::async_runtime::block_on(secrets.meeting_storage_key()).unwrap();
    let mut connection = open_encrypted_connection(&root.join("meeting-store.db"), &key).unwrap();
    migrations
        .to_version(&mut connection, with_folders - 1)
        .unwrap();
    assert!(!has_table(&connection, "meeting_folders"));
    let session_id = MeetingSessionId::new();
    connection
        .execute(
            "INSERT INTO meeting_sessions (
                id, phase, revision, title, origin_kind, preflight_json,
                created_at_utc_ms, started_at_utc_ms, processing_status,
                retention_policy_json
             ) VALUES (?1, 'review_ready', 0, 'Acme kickoff', 'manual', '{}', 1000, 1000,
                       '{\"kind\":\"succeeded\"}', '{\"kind\":\"forever\"}')",
            params![session_id.uuid().to_string()],
        )
        .unwrap();
    drop(connection);

    let key = tauri::async_runtime::block_on(secrets.meeting_storage_key()).unwrap();
    let store = MeetingStore::open(root, key).unwrap();

    assert_eq!(listed(&store, None), vec![session_id]);
    assert!(store.meeting_folder_ids(session_id).unwrap().is_empty());
    let created = create(&store, "Clients", 0);
    let clients = folder_named(&created, "Clients");
    let filed = file(&store, clients, session_id, created.folders.revision);
    assert_eq!(filed.receipt.result, OperationResult::Committed);
    assert_eq!(listed(&store, Some(clients)), vec![session_id]);
    assert_eq!(store.meeting_folder_ids(session_id).unwrap(), vec![clients]);
}

/// A folder holds references: one meeting sits in two folders, the list
/// filter shows exactly a folder's meetings, and deleting a folder leaves
/// every meeting that was in it.
#[test]
fn a_folder_holds_references_and_deleting_it_keeps_the_meetings() {
    let (_directory, store) = store();
    let kickoff = reviewable_meeting(&store, "Acme kickoff", 1_000);
    let standup = reviewable_meeting(&store, "Standup", 2_000);
    let created = create(&store, "Q3", 0);
    let created = create(&store, "Clients", created.folders.revision);
    let (q3, clients) = (
        folder_named(&created, "Q3"),
        folder_named(&created, "Clients"),
    );
    assert_eq!(
        created
            .folders
            .folders
            .iter()
            .map(|folder| folder.name.as_str())
            .collect::<Vec<_>>(),
        ["Clients", "Q3"],
        "folders are listed by name"
    );

    let filed = file(&store, clients, kickoff, created.folders.revision);
    let filed = file(&store, q3, kickoff, filed.folders.revision);
    let filed = file(&store, q3, kickoff, filed.folders.revision);
    assert_eq!(
        filed.folders.folders[1].meeting_count, 1,
        "filing a meeting twice files it once"
    );
    assert_eq!(listed(&store, Some(clients)), vec![kickoff]);
    assert_eq!(listed(&store, None), vec![standup, kickoff]);
    assert_eq!(
        store.meeting_folder_ids(kickoff).unwrap(),
        vec![clients, q3]
    );

    let stale = store
        .delete_meeting_folder(
            &MeetingFolderDeleteRequest {
                operation_id: MeetingOperationId::new(),
                folder_id: clients,
                expected_revision: created.folders.revision,
            },
            3,
        )
        .unwrap();
    assert_eq!(stale.receipt.result, OperationResult::Rejected);
    assert_eq!(
        stale.folders.folders.len(),
        2,
        "a stale write changes nothing"
    );

    let deleted = store
        .delete_meeting_folder(
            &MeetingFolderDeleteRequest {
                operation_id: MeetingOperationId::new(),
                folder_id: clients,
                expected_revision: filed.folders.revision,
            },
            3,
        )
        .unwrap();
    assert_eq!(deleted.receipt.result, OperationResult::Committed);
    assert_eq!(listed(&store, None), vec![standup, kickoff]);
    assert_eq!(store.meeting_folder_ids(kickoff).unwrap(), vec![q3]);

    assert_eq!(
        store
            .create_meeting_folder(
                &MeetingFolderCreateRequest {
                    operation_id: MeetingOperationId::new(),
                    name: " q3 ".to_string(),
                    expected_revision: deleted.folders.revision,
                },
                4,
            )
            .unwrap_err(),
        StoreError::Invalid,
        "two folders may not share a name, whatever its case"
    );
}
