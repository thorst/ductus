-- Status per destination. Each queued message gets one row per destination of its interface;
-- each destination works through its own rows, so one that's down holds up only itself.
-- `messages.status` stays the message's overall status (pending until every destination is
-- done), which is also what older binaries read.

-- Every destination the engine has been configured with, by interface and name. Deliveries
-- refer to it by a small id: names repeated in every row (and both its indexes) halved bulk
-- ingest speed.
CREATE TABLE destinations (
    id        integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    interface text NOT NULL,
    name      text NOT NULL,
    UNIQUE (interface, name)
);

CREATE TABLE deliveries (
    message_id     bigint      NOT NULL,
    received_at    timestamptz NOT NULL,
    destination_id integer     NOT NULL,
    partition_key  integer     NOT NULL,
    -- 0 pending, 1 sent, 2 filtered, 3 quarantined (as messages.status)
    status         smallint    NOT NULL DEFAULT 0,
    attempts       integer     NOT NULL DEFAULT 0,
    last_error     text,
    -- What was sent, only for destinations with `save_output: true`.
    output         bytea,
    updated_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (message_id, received_at, destination_id)
) PARTITION BY RANGE (received_at);

-- Each destination's work queue, per lane, in arrival order.
CREATE INDEX deliveries_pending ON deliveries (destination_id, partition_key, message_id) WHERE status = 0;

-- Daily partitions for both tables. Deliveries get a partition for every messages partition
-- (older ones too), so pending messages from any day can be given their deliveries.
CREATE OR REPLACE FUNCTION ductus_ensure_partitions(days_ahead integer) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    d date;
    p record;
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
    -- Same bounds as the messages partition (copied, so the session's time zone can't shift them).
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

SELECT ductus_ensure_partitions(3);
