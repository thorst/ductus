-- Upgrade: adds tables that keep the releases stored on this database (`POST /releases`) and each check of one against an environment; nothing existing changes
-- Heavy: none
--
-- 0.7.4.3.3: a release (`ductus release`, `ductus_runtime::release`) stored in the database, so
-- every engine on it and the web UI see it, and a deploy (0.7.4.4) or a rollback is choosing one.
--
-- Stored without repeating what earlier releases or code versions already hold: each text file once
-- by its content (in `code_files`, shared with code versions: the same normalized text, the same
-- hash), each file that isn't text once (`release_blobs`), and each folder's list of files once
-- (`release_folders`): a release names its folders. At 1,001 interfaces a release is ~1,000 folder
-- hashes; one that changed ten interfaces adds ten folders and their changed files.

-- Files a release carries that aren't text (a NUL byte, or not UTF-8), once by their content.
CREATE TABLE release_blobs (
    -- SHA-256 of `content`.
    hash    bytea PRIMARY KEY,
    content bytea NOT NULL
);

-- A folder's files, once by the list's content: an interface's folder (`interfaces/<name>`), a
-- folder at the project's top (`lib`, `environments`), or the project's top files themselves ("").
CREATE TABLE release_folders (
    -- SHA-256 of the list, as `ductus_store::releases` writes it.
    hash  bytea PRIMARY KEY,
    -- `[{"path": "interfaces/a/interface.yaml", "hash": "<hex>"}, {"path": "...", "hash": "...",
    -- "binary": true}]`, in path order, paths relative to the project: a text's content is in
    -- `code_files`, a binary file's in `release_blobs`.
    files jsonb NOT NULL
);

CREATE TABLE releases (
    -- The release id (SHA-256 over its files: `ductus_runtime::release`), recomputed from the files
    -- when it was stored.
    id        bytea       PRIMARY KEY,
    -- The project's folder name, and its manifest (`release.json`) as received.
    project   text        NOT NULL,
    manifest  jsonb       NOT NULL,
    -- From the manifest, for listing releases without reading each one's (hundreds of KB at a
    -- thousand interfaces): who made it, when and why, from which commit (null without git), how
    -- many interfaces and files it carries and their bytes unpacked.
    made_at    timestamptz,
    made_by    text        NOT NULL,
    reason     text,
    git_commit text,
    interfaces integer     NOT NULL,
    files      integer     NOT NULL,
    bytes      bigint      NOT NULL,
    -- The commit's ancestry: 20-byte commit ids, sorted (empty without git): what a deploy's
    -- "replaces" check (0.7.4.4) reads. 20 KB at 1,000 commits, 2 MB at 100,000.
    ancestry  bytea       NOT NULL,
    -- Its folders (`release_folders.hash`), in path order.
    folders   bytea[]     NOT NULL,
    -- When it was stored here and by whom (as the audit log names them).
    stored_at timestamptz NOT NULL DEFAULT now(),
    stored_by text        NOT NULL
);
CREATE INDEX releases_stored_at ON releases (stored_at);

-- Each check of a release against an environment (`POST /releases/{id}/check`): the gate's report.
CREATE TABLE release_checks (
    id          integer     GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    release_id  bytea       NOT NULL REFERENCES releases (id),
    at          timestamptz NOT NULL DEFAULT now(),
    -- The environment checked against (the engine's), and the engine that ran it.
    environment text        NOT NULL,
    instance    text        NOT NULL,
    host        text        NOT NULL,
    by_name     text        NOT NULL,
    ok          boolean     NOT NULL,
    problems    integer     NOT NULL,
    seconds     real        NOT NULL,
    -- `ductus release check --json`'s document, without the manifest (it's the release's).
    report      jsonb       NOT NULL
);
CREATE INDEX release_checks_release ON release_checks (release_id, at);
