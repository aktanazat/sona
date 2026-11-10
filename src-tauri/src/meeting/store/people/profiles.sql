ALTER TABLE persons ADD COLUMN role TEXT;
ALTER TABLE persons ADD COLUMN notes TEXT;
ALTER TABLE persons ADD COLUMN organization_pinned INTEGER NOT NULL DEFAULT 0 CHECK (organization_pinned IN (0, 1));
