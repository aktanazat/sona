CREATE TABLE person_contact_cards (
    person_id TEXT PRIMARY KEY NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    card_json TEXT NOT NULL CHECK (length(card_json) > 0 AND length(card_json) <= 65536),
    updated_at_utc_ms INTEGER NOT NULL
);
