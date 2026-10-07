-- Partition maintenance in two steps, each its own transaction (the engine calls them as two
-- statements). `ductus_ensure_partitions` created a day's `messages` partition and then its
-- `deliveries` partition in one transaction: it held a lock on `messages` while waiting for
-- `deliveries`, and settling a message status holds `deliveries` while waiting for `messages`,
-- so the two could deadlock (Postgres then aborts one). Holding one table's lock at a time,
-- maintenance can only make others wait.
--
-- `ductus_ensure_partitions` is kept unchanged for older binaries (expand-only migrations).

CREATE OR REPLACE FUNCTION ductus_ensure_message_partitions(days_ahead integer) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    d date;
BEGIN
    FOR i IN -1..days_ahead LOOP
        d := current_date + i;
        BEGIN
            EXECUTE format(
                'CREATE TABLE IF NOT EXISTS %I PARTITION OF messages FOR VALUES FROM (%L) TO (%L)',
                'messages_' || to_char(d, 'YYYYMMDD'), d, d + 1);
        EXCEPTION WHEN duplicate_table OR unique_violation THEN
            NULL; -- another session created it first
        END;
    END LOOP;
END $$;

-- A `deliveries` partition for every `messages` partition that lacks one, with the same bounds
-- (copied, so the session's time zone can't shift them). Run after the messages step: until
-- then a new day has messages but not deliveries, which is harmless for days ahead (nothing is
-- stored there yet) and at startup (nothing is received before maintenance has run).
CREATE OR REPLACE FUNCTION ductus_ensure_delivery_partitions() RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    p record;
BEGIN
    FOR p IN
        SELECT 'deliveries_' || substr(c.relname, 10) AS name, pg_get_expr(c.relpartbound, c.oid) AS bounds
        FROM pg_inherits i JOIN pg_class c ON c.oid = i.inhrelid
        WHERE i.inhparent = 'messages'::regclass AND c.relname ~ '^messages_[0-9]{8}$'
          AND to_regclass('deliveries_' || substr(c.relname, 10)) IS NULL
    LOOP
        BEGIN
            EXECUTE format('CREATE TABLE IF NOT EXISTS %I PARTITION OF deliveries %s', p.name, p.bounds);
        EXCEPTION WHEN duplicate_table OR unique_violation THEN
            NULL;
        END;
    END LOOP;
END $$;
