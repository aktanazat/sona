-- Typeless's typeless.db: the legacy history table and history_v2 (v1.8.0+).
-- DDL after Hoshiro's importer test; rows are synthetic.
-- https://github.com/IcyBlue17/Hoshiro (src-tauri/src/importer.rs)
-- https://github.com/UllrAI/SayKuku (Scripts/export-typeless.py)
CREATE TABLE history (id text PRIMARY KEY, status text, mode text, refined_text text, duration real, created_at text, mode_meta blob, focused_app_name text, focused_app_bundle_id text, focused_app_window_title text);
CREATE TABLE history_v2 (id text PRIMARY KEY, status text, mode text, refined_text text, duration real, created_at text, mode_meta blob);
INSERT INTO history VALUES ('old', 'completed', 'voice_translation', 'Hi', 1.0, '2026-01-01T00:00:00.000Z', NULL, 'Mail', 'com.apple.mail', NULL);
INSERT INTO history VALUES ('a', 'completed', 'voice_transcript', 'Hello wrld.', 2.5, '2026-10-09T18:57:31.104Z', NULL, 'Notes', 'com.apple.Notes', NULL);
INSERT INTO history_v2 VALUES ('a', 'completed', 'voice_transcript', 'Hello world.', 2.5, '2026-10-09T18:57:31.104Z', NULL);
INSERT INTO history_v2 VALUES ('b', 'completed', 'voice_command', NULL, 1.0, '2026-10-09T19:00:00.000Z', CAST('{"ai_result":{"user_prompt":"translate hello to spanish","refined_text":"Hola"}}' AS BLOB));
INSERT INTO history_v2 VALUES ('c', 'dismissed', 'voice_transcript', NULL, 0.5, '2026-10-09T16:58:51.611Z', NULL);
