ALTER TABLE meeting_calendar_facts
    ADD COLUMN found_under_way INTEGER NOT NULL DEFAULT 0 CHECK (found_under_way IN (0, 1));
