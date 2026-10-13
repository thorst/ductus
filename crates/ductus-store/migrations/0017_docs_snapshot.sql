-- The project's documentation as the company wrote it (v0.5 step 7, part 5): for each interface
-- and each resources.yaml entry, `about` and `custom` (docs-schema.yaml's fields) with its
-- destinations' `custom`, in the company's own field names, so SQL reports and BI tools can ask
-- "which interfaces have no signed BAA" without reading YAML. The YAML in Git is the source of
-- truth: the engine replaces the rows at every start (entries no longer in the project are
-- removed). Versioned history comes with versioned releases (v0.6).
CREATE TABLE docs_snapshot (
    kind       text        NOT NULL CHECK (kind IN ('interface', 'resource')),
    name       text        NOT NULL,
    docs       jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (kind, name)
);
CREATE INDEX docs_snapshot_docs ON docs_snapshot USING gin (docs);
