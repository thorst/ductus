-- Messages, partitioned by day so retention is a cheap DROP of whole partitions.
CREATE TABLE messages (
    id            bigint      GENERATED ALWAYS AS IDENTITY,
    received_at   timestamptz NOT NULL DEFAULT now(),
    interface     text        NOT NULL,
    -- 0 pending, 1 sent, 2 filtered, 3 quarantined
    status        smallint    NOT NULL DEFAULT 0,
    partition_key integer     NOT NULL,
    ordering_key  text,
    control_id    text,
    payload       bytea       NOT NULL,
    attempts      integer     NOT NULL DEFAULT 0,
    last_error    text,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id, received_at)
) PARTITION BY RANGE (received_at);

-- The work queue: pending messages per interface and lane, in arrival order.
CREATE INDEX messages_pending ON messages (interface, partition_key, id) WHERE status = 0;

-- Create daily partitions from yesterday through `days_ahead` days from now.
CREATE FUNCTION ductus_ensure_partitions(days_ahead integer) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    d date;
BEGIN
    FOR i IN -1..days_ahead LOOP
        d := current_date + i;
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF messages FOR VALUES FROM (%L) TO (%L)',
            'messages_' || to_char(d, 'YYYYMMDD'), d, d + 1);
    END LOOP;
END $$;
