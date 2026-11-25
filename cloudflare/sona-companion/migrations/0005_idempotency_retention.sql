-- sona-migration-version: 0005_idempotency_retention
-- sona-migration-checksum: sha256:8889299028bc834667f68fd3cee7cec68c170a8324a7b6ad13739c2485d28f87
CREATE INDEX idempotency_results_retention ON idempotency_results(created_at);

INSERT INTO schema_migrations (version, checksum, applied_at)
VALUES ('0005_idempotency_retention', 'sha256:8889299028bc834667f68fd3cee7cec68c170a8324a7b6ad13739c2485d28f87', unixepoch() * 1000);
