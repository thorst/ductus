-- Upgrade: adds what `ductus history` reads (which interfaces each start ran, the documentation's changes per deployment, a warning per change); copies today's documentation snapshot once as its first history
-- Heavy: docs_snapshot: every row copied once into docs_history (one per interface and resource: thousands at most); starts wait meanwhile
-- Heavy: code_versions: an index built (a row per version an interface ran); starts wait meanwhile
-- Heavy: deployments: an index built (a row per start); starts wait meanwhile
-- Heavy: audit_log: an index built (every row read once); what the audit log records (sign-ins, payload reads, resends, pauses) waits meanwhile, messages don't
--
-- 0.7.3.3: one timeline of who changed what and why, per interface: its git history, the
-- deployments that changed it, the audit log's entries naming it and its documentation's changes.
--
-- No `lock_timeout` here: no table here is written by ingest or delivery. Most are written once per
-- engine start, so the ALTERs wait at most for another engine's start to commit; the audit log's
-- index waits for no one (building it lets reads on, makes writers wait: seconds per million rows),
-- and waiting is better than failing an upgrade for that.

-- Which interfaces a start ran, once per distinct set (a start that runs what the last one ran adds
-- nothing), so `history --all` can show the starts that ran an interface without changing it.
CREATE TABLE interface_sets (
    id         integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- SHA-256 of the names, sorted, each followed by `\n`.
    hash       bytea   NOT NULL UNIQUE,
    interfaces text[]  NOT NULL
);
CREATE INDEX interface_sets_interfaces ON interface_sets USING gin (interfaces);

-- Null for deployments recorded before 0.7.3.3 (or by an older engine).
ALTER TABLE deployments ADD COLUMN interface_set integer REFERENCES interface_sets (id);
CREATE INDEX deployments_interface_set ON deployments (interface_set, id);

-- Why this change may undo someone else's: the version it replaced came from a commit that isn't
-- in this one's history, or from files never committed, deployed by someone else (folder mode:
-- two people deploying from their own checkouts). Null when there's nothing to say.
ALTER TABLE deployment_changes ADD COLUMN warning text;

-- `history` finds every version an interface ran, for the files they name.
CREATE INDEX code_versions_interface ON code_versions (interface);

-- `history` reads an interface's changes in the audit log without its payload reads (and refused
-- attempts), which can be most of its rows: an index of the rest.
CREATE INDEX audit_log_interface_changes ON audit_log (interface, at)
    WHERE interface IS NOT NULL AND action <> 'message.payload.read' AND outcome = 'ok';

-- The documentation's history: a row each time an entry of `docs_snapshot` changed, appeared or
-- went, with the deployment that did it.
CREATE TABLE docs_history (
    id            bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Null: from before 0.7.3.3 (the snapshot as it was at the upgrade), or a start whose
    -- deployment couldn't be recorded.
    deployment_id integer     REFERENCES deployments (id) ON DELETE SET NULL,
    at            timestamptz NOT NULL DEFAULT now(),
    kind          text        NOT NULL CHECK (kind IN ('interface', 'resource')),
    name          text        NOT NULL,
    -- As in `docs_snapshot`; null: the entry left the project.
    docs          jsonb
);
CREATE INDEX docs_history_entry ON docs_history (kind, name, id);

-- Today's snapshot is where each entry's history starts.
INSERT INTO docs_history (at, kind, name, docs)
SELECT updated_at, kind, name, docs FROM docs_snapshot ORDER BY kind, name;
