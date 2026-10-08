-- Retention (v0.5 step 5): each interface keeps its messages for one of four fixed windows,
-- 7, 21, 45 or 90 days (its tier), and old messages go by dropping whole partitions, never by
-- deleting rows from the busy tables. Longer history belongs to the source systems or an
-- archive: the engine isn't one.
--
-- `messages` and `deliveries` are partitioned by tier first, then by time: a day for 7d, a week
-- for the others. That's about 38 partitions per table. The tiers and periods are chosen for
-- partition count, which statements Postgres can't prune (the queue read, settling, the message
-- browser) pay for: days within tiers would be ~690 partitions, and the queue read took 20 ms
-- instead of 6 ms (measured; ductus-enterprise decisions.md "Retention").
--
-- A partitioned table can't be re-partitioned, so the tables are made again and the rows
-- copied (into the 90d tier, the longest: they were kept forever until now, and the engine
-- moves each interface's rows to its own tier on first start). Older binaries keep working: `tier` has a
-- default and the partition functions they call keep their names.

ALTER TABLE messages RENAME TO messages_old;
ALTER INDEX messages_pkey RENAME TO messages_old_pkey;
ALTER INDEX messages_pending RENAME TO messages_old_pending;
ALTER INDEX messages_by_interface RENAME TO messages_old_by_interface;
ALTER INDEX messages_quarantined RENAME TO messages_old_quarantined;
ALTER SEQUENCE messages_id_seq RENAME TO messages_old_id_seq;
ALTER TABLE deliveries RENAME TO deliveries_old;
ALTER INDEX deliveries_pkey RENAME TO deliveries_old_pkey;
ALTER INDEX deliveries_pending RENAME TO deliveries_old_pending;
ALTER INDEX deliveries_quarantined RENAME TO deliveries_old_quarantined;

CREATE TABLE messages (
    id            bigint      GENERATED ALWAYS AS IDENTITY,
    received_at   timestamptz NOT NULL DEFAULT now(),
    interface     text        NOT NULL,
    -- 0 pending, 1 sent, 2 filtered, 3 quarantined, 4 duplicate
    status        smallint    NOT NULL DEFAULT 0,
    partition_key integer     NOT NULL,
    ordering_key  text,
    control_id    text,
    payload       bytea       NOT NULL,
    attempts      integer     NOT NULL DEFAULT 0,
    last_error    text,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    origin_id     bigint,
    -- The retention tier the row is stored in, in days. 90 for rows stored by older binaries,
    -- which don't know tiers: kept longest, and moved to their interface's tier by the engine.
    tier          smallint    NOT NULL DEFAULT 90,
    PRIMARY KEY (id, received_at, tier)
) PARTITION BY LIST (tier);
CREATE INDEX messages_pending ON messages (interface, partition_key, id) WHERE status = 0;
CREATE INDEX messages_by_interface ON messages (interface, id);
CREATE INDEX messages_quarantined ON messages (interface, id) WHERE status = 3;

CREATE TABLE deliveries (
    message_id     bigint      NOT NULL,
    received_at    timestamptz NOT NULL,
    destination_id integer     NOT NULL,
    partition_key  integer     NOT NULL,
    -- 0 pending, 1 sent, 2 filtered, 3 quarantined (as messages.status)
    status         smallint    NOT NULL DEFAULT 0,
    attempts       integer     NOT NULL DEFAULT 0,
    last_error     text,
    output         bytea,
    updated_at     timestamptz NOT NULL DEFAULT now(),
    -- Always its message's tier (they move together).
    tier           smallint    NOT NULL DEFAULT 90,
    PRIMARY KEY (message_id, received_at, destination_id, tier)
) PARTITION BY LIST (tier);
CREATE INDEX deliveries_pending ON deliveries (destination_id, partition_key, message_id) WHERE status = 0;
CREATE INDEX deliveries_quarantined ON deliveries (destination_id, message_id) WHERE status = 3;

-- The tiers, in days. Adding one is a migration (a new pair of tier tables).
CREATE FUNCTION ductus_tiers() RETURNS integer[] IMMUTABLE LANGUAGE sql AS $$ SELECT '{7,21,45,90}'::integer[] $$;

-- How much time one partition of a tier covers. A message is gone at most a period after its
-- window (a 45d message within a week after). The 7d tier is daily: weekly would keep its
-- messages up to twice their window.
CREATE FUNCTION ductus_tier_unit(tier integer) RETURNS text IMMUTABLE LANGUAGE sql AS $$
    SELECT CASE WHEN tier <= 7 THEN 'day' ELSE 'week' END
$$;

DO $$
DECLARE t integer;
BEGIN
    FOREACH t IN ARRAY ductus_tiers() LOOP
        EXECUTE format('CREATE TABLE messages_t%s PARTITION OF messages FOR VALUES IN (%s) PARTITION BY RANGE (received_at)', t, t);
        EXECUTE format('CREATE TABLE deliveries_t%s PARTITION OF deliveries FOR VALUES IN (%s) PARTITION BY RANGE (received_at)', t, t);
    END LOOP;
END $$;

-- The partition of `tab` (`messages` or `deliveries`) for tier `tier` that holds `day`, created
-- if it's missing; returns its name, e.g. `messages_t30_20261008` (named by its first day).
-- Periods start at midnight UTC whatever the session's time zone (a date in the bounds would be
-- read in it, and two sessions in different zones made overlapping partitions).
CREATE FUNCTION ductus_create_tier_partition(tab text, tier integer, day date) RETURNS text
LANGUAGE plpgsql AS $$
DECLARE
    unit  text := ductus_tier_unit(tier);
    first date := date_trunc(unit, day::timestamp)::date;
    name  text := format('%s_t%s_%s', tab, tier, to_char(first, 'YYYYMMDD'));
BEGIN
    IF to_regclass(name) IS NULL THEN
        BEGIN
            EXECUTE format('CREATE TABLE %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
                name, tab || '_t' || tier, first::timestamp AT TIME ZONE 'UTC',
                (first + ('1 ' || unit)::interval) AT TIME ZONE 'UTC');
        EXCEPTION WHEN duplicate_table OR unique_violation THEN
            NULL; -- another session created it first
        END;
    END IF;
    RETURN name;
END $$;

-- Locking: creating a partition locks its tier's table (`messages_t30`) exclusively. Queries
-- that can't prune by tier lock every tier's table, shortest tier first (they were created in
-- that order, and Postgres locks partitions in creation (OID) order). So maintenance takes one
-- tier per transaction (the engine), or the tiers in that same order (the functions older
-- binaries call): holding the 45d table while waiting for the 7d one would deadlock with a
-- query holding the 7d one (it did, in the store stress test).

-- One tier's partitions of `tab` from yesterday to `days_ahead` days ahead.
CREATE FUNCTION ductus_ensure_tier_partitions(tab text, tier integer, days_ahead integer) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    FOR i IN -1..days_ahead LOOP
        PERFORM ductus_create_tier_partition(tab, tier, current_date + i);
    END LOOP;
END $$;

-- What older binaries call (0006): every tier's `messages` partitions, shortest tier first.
CREATE OR REPLACE FUNCTION ductus_ensure_message_partitions(days_ahead integer) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE t integer;
BEGIN
    FOREACH t IN ARRAY ductus_tiers() LOOP
        PERFORM ductus_ensure_tier_partitions('messages', t, days_ahead);
    END LOOP;
END $$;

-- A `deliveries` partition of tier `tier` for every `messages` partition of that tier that lacks
-- one, with the same bounds (copied, so the session's time zone can't shift them). Partitions
-- dropped mid-scan are skipped (0007).
CREATE FUNCTION ductus_ensure_delivery_partitions(tier integer) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    p record;
BEGIN
    FOR p IN
        SELECT 'deliveries' || substr(c.relname, 9) AS name, pg_get_expr(c.relpartbound, c.oid) AS bounds
        FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
        WHERE i.inhparent = to_regclass('messages_t' || tier) AND c.relname ~ '^messages_t[0-9]+_[0-9]{8}$'
          AND to_regclass('deliveries' || substr(c.relname, 9)) IS NULL
    LOOP
        CONTINUE WHEN p.bounds IS NULL;
        BEGIN
            EXECUTE format('CREATE TABLE IF NOT EXISTS %I PARTITION OF %I %s', p.name, 'deliveries_t' || tier, p.bounds);
        EXCEPTION WHEN duplicate_table OR unique_violation THEN
            NULL;
        END;
    END LOOP;
END $$;

-- What older binaries call (0006): every tier, shortest first.
CREATE OR REPLACE FUNCTION ductus_ensure_delivery_partitions() RETURNS void
LANGUAGE plpgsql AS $$
DECLARE t integer;
BEGIN
    FOREACH t IN ARRAY ductus_tiers() LOOP
        PERFORM ductus_ensure_delivery_partitions(t);
    END LOOP;
END $$;

-- What the oldest binaries call (0005).
CREATE OR REPLACE FUNCTION ductus_ensure_partitions(days_ahead integer) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM ductus_ensure_message_partitions(days_ahead);
    PERFORM ductus_ensure_delivery_partitions();
END $$;

-- The rows so far, into the 90d tier.
SELECT ductus_create_tier_partition('messages', 90, day)
FROM (SELECT DISTINCT received_at::date AS day FROM messages_old) d;
SELECT ductus_ensure_message_partitions(3);
SELECT ductus_ensure_delivery_partitions();
INSERT INTO messages (id, received_at, interface, status, partition_key, ordering_key, control_id, payload, attempts,
                      last_error, updated_at, origin_id, tier)
OVERRIDING SYSTEM VALUE
SELECT id, received_at, interface, status, partition_key, ordering_key, control_id, payload, attempts,
       last_error, updated_at, origin_id, 90
FROM messages_old;
INSERT INTO deliveries (message_id, received_at, destination_id, partition_key, status, attempts, last_error, output,
                        updated_at, tier)
SELECT message_id, received_at, destination_id, partition_key, status, attempts, last_error, output, updated_at, 90
FROM deliveries_old;
-- New ids carry on where the old ones stopped.
SELECT setval(pg_get_serial_sequence('messages', 'id'), last_value, is_called) FROM messages_old_id_seq;
DROP TABLE deliveries_old;
DROP TABLE messages_old;

-- Each interface's retention as the engine applied it. A change (`changing_from` set) runs in
-- the background: the interface's rows in other tiers are moved into `days`, or deleted when
-- they're past its window, in small batches. One change at a time: a value configured while one
-- runs, or a shorter one not yet confirmed (shortening deletes messages), waits in `waiting`.
CREATE TABLE retention (
    interface     text        PRIMARY KEY,
    -- The tier its new messages are stored in, in days (the target of a running change).
    days          smallint    NOT NULL,
    -- Set while a change runs: the tier it's changing from.
    changing_from smallint,
    -- Rows of the interface still in other tiers (an estimate, counted when the change starts).
    rows_left     bigint,
    -- A configured value not applied yet.
    waiting       smallint,
    -- A shorter `waiting` value someone confirmed.
    confirmed     smallint,
    changed_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
