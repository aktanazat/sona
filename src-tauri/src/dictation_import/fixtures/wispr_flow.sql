-- Wispr Flow's flow.sqlite, the History and Dictionary columns the importer
-- reads. DDL after wispr-cli's and WisprMCP's published schemas; rows are
-- synthetic.
-- https://github.com/pedramamini/WisprMCP/blob/main/wispr_flow_database_schema.md
-- https://github.com/iamnikolie/wispr-cli
CREATE TABLE History (
    transcriptEntityId VARCHAR(36) PRIMARY KEY,
    asrText TEXT,
    formattedText TEXT,
    editedText TEXT,
    timestamp DATETIME,
    status VARCHAR(255),
    app VARCHAR(255),
    url TEXT,
    numWords INTEGER,
    duration FLOAT,
    isArchived TINYINT DEFAULT 0,
    language TEXT,
    detectedLanguage TEXT,
    audio BLOB
);
CREATE TABLE Dictionary (
    id VARCHAR(36) PRIMARY KEY,
    phrase TEXT,
    replacement TEXT,
    isSnippet TINYINT DEFAULT 0,
    isStarred TINYINT DEFAULT 0,
    source TEXT,
    frequencyUsed INTEGER DEFAULT 0,
    remoteFrequencyUsed INTEGER DEFAULT 0,
    lastUsed DATETIME,
    isDeleted TINYINT DEFAULT 0
);
INSERT INTO History VALUES ('a1b2c3d4-0000-4000-8000-000000000001', 'ship it friday', 'Ship it Friday.', NULL, '2026-11-11 09:00:00.123 +00:00', 'formatted', 'com.tinyspeck.slackmacgap', '', 3, 3.5, 0, 'en', 'en', NULL);
INSERT INTO History VALUES ('a1b2c3d4-0000-4000-8000-000000000002', 'old one', 'Old one.', NULL, '2026-11-11 10:30:00.000 +00:00', 'formatted', 'md.obsidian', '', 2, 2, 1, NULL, NULL, NULL);
INSERT INTO History VALUES ('a1b2c3d4-0000-4000-8000-000000000003', 'never mind', '', NULL, '2026-11-11 11:00:00.000 +00:00', 'dismissed', 'com.apple.Notes', '', 2, 1.5, 0, 'en', 'en', NULL);
INSERT INTO History VALUES ('a1b2c3d4-0000-4000-8000-000000000004', 'lets meet at noon', 'Let''s meet at noon.', 'Let''s meet at 12:30.', '2026-11-11 12:00:00.000 +00:00', 'formatted', 'com.apple.mail', '', 4, 2, 0, 'en', 'en', NULL);
INSERT INTO Dictionary VALUES ('k1', 'Sona', '', 0, 1, 'manual', 3, 0, '2026-09-01 10:00:00.000 +00:00', 0);
INSERT INTO Dictionary VALUES ('k2', 'jason', 'JSON', 0, 0, 'user_edits', 1, 0, NULL, 0);
INSERT INTO Dictionary VALUES ('k3', 'btw', 'by the way', 1, 0, 'manual', 0, 5, NULL, 0);
INSERT INTO Dictionary VALUES ('k4', 'gone', '', 0, 0, 'manual', 9, 0, NULL, 1);
