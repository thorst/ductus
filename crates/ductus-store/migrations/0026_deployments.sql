-- Upgrade: adds tables that record what code each start and handover runs (code versions, their files, deployments); nothing existing changes
-- Heavy: none
--
-- 0.7.3.1: every start and handover records what each interface runs (its code version: a hash of
-- its interface.yaml and every file its code loaded), from which git commit, by whom and why.

-- Each file any code version has had, once by its content: the text kept, so the code that ran can
-- be shown (and run again) without git. Postgres compresses long values itself.
CREATE TABLE code_files (
    -- SHA-256 of `content`.
    hash    bytea PRIMARY KEY,
    -- The text as hashed: no byte order mark, `\n` line ends.
    content text  NOT NULL
);

-- What an interface ran: one row per distinct version (a start that changes nothing adds none).
CREATE TABLE code_versions (
    id         integer     GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The version (`ductus_runtime::code`): shown as its first 12 hex characters.
    hash       bytea       NOT NULL UNIQUE,
    interface  text        NOT NULL,
    -- `[{"path": "interfaces/a/interface.yaml", "hash": "<hex>"}, ...]`, in path order; each file's
    -- text is in `code_files`.
    files      jsonb       NOT NULL,
    first_seen timestamptz NOT NULL DEFAULT now()
);

-- One row per start and handover, whether or not anything changed.
CREATE TABLE deployments (
    id           integer     GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at           timestamptz NOT NULL DEFAULT now(),
    environment  text        NOT NULL,
    -- The engine run (as in the audit log's `engine.start`), and its machine.
    instance     text        NOT NULL,
    host         text        NOT NULL,
    -- The binary's version.
    version      text        NOT NULL,
    -- `start` (someone ran it), `service` (systemd or the Windows service started it), `handover`.
    how          text        NOT NULL,
    -- Who: the account that started it, the person who asked for the handover (`sudo ductus
    -- handover`), or `systemd (systemctl reload)`.
    by_name      text        NOT NULL,
    -- Why (`ductus handover -m`); null when nobody said.
    reason       text,
    -- HEAD's commit (hex); null when the project isn't in a git repository or it has no commits.
    git_commit   text,
    -- Files the interfaces run that aren't the commit's (changed since, or not in it); null without git.
    uncommitted  text[],
    -- How many interfaces it runs, and how many of them run a version they didn't before.
    interfaces   integer     NOT NULL,
    changed      integer     NOT NULL
);
CREATE INDEX deployments_at ON deployments (at);

-- The interfaces a deployment changed: from what to what (`from_version` null: new here).
CREATE TABLE deployment_changes (
    deployment_id integer NOT NULL REFERENCES deployments (id) ON DELETE CASCADE,
    interface     text    NOT NULL,
    from_version  integer REFERENCES code_versions (id),
    to_version    integer NOT NULL REFERENCES code_versions (id),
    PRIMARY KEY (deployment_id, interface)
);
CREATE INDEX deployment_changes_interface ON deployment_changes (interface, deployment_id);

-- What each interface runs now (its last deployment's), read at every start to tell what changed.
CREATE TABLE interface_versions (
    interface     text    PRIMARY KEY,
    version_id    integer NOT NULL REFERENCES code_versions (id),
    deployment_id integer NOT NULL REFERENCES deployments (id)
);
