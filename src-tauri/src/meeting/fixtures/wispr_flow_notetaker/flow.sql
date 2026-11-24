-- Wispr Flow Notetaker's Meetings table in flow.sqlite. DDL after
-- wispr-cli's models and test schema; rows are synthetic.
-- https://github.com/iamnikolie/wispr-cli (internal/store/models.go)
CREATE TABLE Meetings (id TEXT PRIMARY KEY, title TEXT, createdAt DATETIME, modifiedAt DATETIME, endedAt INTEGER, recordedMs INTEGER, participantNames TEXT, summary TEXT, notes TEXT, speakerMap TEXT, refineStatus TEXT, importSource TEXT, shareSlug TEXT, isDeleted TINYINT DEFAULT 0, isTourDemo TINYINT DEFAULT 0);
INSERT INTO Meetings VALUES ('aaaa1111-0000-0000-0000-000000000001', 'Planning sync', '2026-09-22 10:00:00.000 +00:00', '2026-09-22 11:00:00.000 +00:00', 1790074800000, 3600000, '["Ana Lee"]', 'Decided to ship Friday. <@speaker:2> owns the release notes.', 'Bring the launch checklist.', '{"people":{"p1":{"name":"Ana Lee"}},"assignments":{"2":{"consensus":"p1"}}}', 'complete', '', '', 0, 0);
INSERT INTO Meetings VALUES ('aaaa1111-0000-0000-0000-000000000002', '', '2026-09-23 15:30:00.000 +00:00', '2026-09-23 16:00:00.000 +00:00', 1790184600000, 1800000, '[]', '', '', NULL, 'pending', '', '', 0, 0);
INSERT INTO Meetings VALUES ('aaaa1111-0000-0000-0000-000000000003', 'No transcript yet', '2026-09-24 09:00:00.000 +00:00', NULL, NULL, NULL, '[]', '', '', NULL, 'pending', '', '', 0, 0);
INSERT INTO Meetings VALUES ('aaaa1111-0000-0000-0000-000000000004', 'Welcome to Notetaker', '2026-09-01 09:00:00.000 +00:00', NULL, NULL, NULL, '[]', '', '', NULL, 'complete', '', '', 0, 1);
INSERT INTO Meetings VALUES ('aaaa1111-0000-0000-0000-000000000005', 'Deleted call', '2026-09-02 09:00:00.000 +00:00', NULL, NULL, NULL, '[]', '', '', NULL, 'complete', '', '', 1, 0);
