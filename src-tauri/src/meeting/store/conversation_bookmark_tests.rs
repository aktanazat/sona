//! Bookmark kinds at the store boundary: a store written before kinds
//! existed reads its bookmarks back as plain marks.

use super::*;
use crate::meeting::conversation_review::ConversationBookmarkKind;
use crate::secrets::SecretManager;
use tempfile::TempDir;

fn has_kind(connection: &Connection) -> bool {
    // PANIC: a fixture; a probe that fails should fail the test that asked.
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('conversation_bookmarks') WHERE name = 'kind')",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn bookmarks_saved_before_kinds_read_back_as_marks() {
    let migrations = Migrations::new(MIGRATIONS.to_vec());
    let mut probe = Connection::open_in_memory().unwrap();
    let with_kinds = (1..=MIGRATIONS.len())
        .find(|&version| {
            migrations.to_version(&mut probe, version).unwrap();
            has_kind(&probe)
        })
        .expect("a migration adds bookmark kinds");

    let directory = TempDir::new().unwrap();
    let secrets = SecretManager::with_backend(Arc::new(crate::secrets::MemorySecretBackend::new()));
    let root = directory.path().join("meetings");
    fs::create_dir_all(&root).unwrap();
    let key = tauri::async_runtime::block_on(secrets.meeting_storage_key()).unwrap();
    let mut connection = open_encrypted_connection(&root.join("meeting-store.db"), &key).unwrap();
    migrations
        .to_version(&mut connection, with_kinds - 1)
        .unwrap();
    assert!(!has_kind(&connection));
    let session_id = MeetingSessionId::new();
    let old = Uuid::new_v4().to_string();
    connection
        .execute(
            "INSERT INTO meeting_sessions (
                id, phase, revision, title, origin_kind, preflight_json,
                created_at_utc_ms, started_at_utc_ms, processing_status,
                retention_policy_json
             ) VALUES (?1, 'review_ready', 0, 'Budget review', 'manual', '{}', 1000, 1000,
                       '{\"kind\":\"succeeded\"}', '{\"kind\":\"forever\"}')",
            params![session_id.uuid().to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO conversation_bookmarks(bookmark_id, session_id, start_offset_ns, label, created_at_utc_ms)
             VALUES (?1, ?2, 5000000000, 'pricing', 1000)",
            params![old, session_id.uuid().to_string()],
        )
        .unwrap();
    drop(connection);

    let key = tauri::async_runtime::block_on(secrets.meeting_storage_key()).unwrap();
    let store = MeetingStore::open(root, key).unwrap();
    let connection = store.connection().unwrap();
    let question = Uuid::new_v4().to_string();
    connection
        .execute(
            "INSERT INTO conversation_bookmarks(bookmark_id, session_id, start_offset_ns, label, kind, created_at_utc_ms)
             VALUES (?1, ?2, 9000000000, NULL, 'question', 2000)",
            params![question, session_id.uuid().to_string()],
        )
        .unwrap();
    assert!(connection
        .execute(
            "INSERT INTO conversation_bookmarks(bookmark_id, session_id, start_offset_ns, label, kind, created_at_utc_ms)
             VALUES (?1, ?2, 1, NULL, 'action', 3000)",
            params![Uuid::new_v4().to_string(), session_id.uuid().to_string()],
        )
        .is_err());
    drop(connection);

    let read = store.conversation_bookmarks(session_id).unwrap();
    let kinds: Vec<_> = read
        .iter()
        .map(|bookmark| {
            (
                bookmark.bookmark_id.as_str(),
                bookmark.label.as_deref(),
                bookmark.kind,
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (
                old.as_str(),
                Some("pricing"),
                ConversationBookmarkKind::Mark
            ),
            (question.as_str(), None, ConversationBookmarkKind::Question),
        ]
    );
}
