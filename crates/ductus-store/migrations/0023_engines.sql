-- 0.6.2: one engine per interface across every engine on this database, and the lane count each
-- interface last ran with (the `ordering.partitions` guard). Older binaries ignore all three
-- tables and claim nothing: a rollback runs without the guards.

-- Every run of an engine. While it runs it holds a session advisory lock with `lock_key` on a
-- connection of its own: that is how other engines know it is alive. Postgres frees the lock
-- however the process ends, so a crash needs no timeout to wait out.
CREATE TABLE engines (
    -- The run's name, as the audit log's `engine.start` and `engine.stop` carry it.
    instance    text        PRIMARY KEY,
    lock_key    bigint      NOT NULL UNIQUE,
    host        text        NOT NULL,
    pid         integer     NOT NULL,
    version     text        NOT NULL,
    environment text        NOT NULL,
    started_at  timestamptz NOT NULL DEFAULT now(),
    -- Set by a clean stop. NULL with the lock not held: it crashed (or lost power).
    stopped_at  timestamptz
);

-- Which engine runs each interface's workers, schedule or file pickup. An engine claims its
-- interfaces before it changes anything for them; a row whose engine no longer holds its lock
-- can be taken over.
CREATE TABLE interface_owners (
    interface text        PRIMARY KEY,
    instance  text        NOT NULL REFERENCES engines ON DELETE CASCADE,
    since     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX interface_owners_instance ON interface_owners (instance);

-- The lanes each interface last ran with: `ordering.partitions`, or 1 without `ordering.key`.
-- Lowering it with messages waiting in the lanes it no longer covers would strand them.
CREATE TABLE interface_applied (
    interface  text        PRIMARY KEY,
    lanes      integer     NOT NULL CHECK (lanes >= 1),
    updated_at timestamptz NOT NULL DEFAULT now()
);
