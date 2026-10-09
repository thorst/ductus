-- The held area (v0.5 step 6e): when a partition expires with messages someone still needs
-- (pending: a destination down or paused for months; quarantined: waiting for a person), those
-- messages move here with their deliveries, and the partition drops as usual. Without it, one
-- waiting message kept its whole week for every interface in the tier: a receiver held for three
-- months doubled the disk, and unresolved quarantines grew it without bound.
--
-- Held rows are tier 0 of `messages` and `deliveries` (`messages_held`, `deliveries_held`),
-- partitioned by month of `received_at` (`messages_held_202609`). A month is dropped whole once
-- nothing in it is unfinished: no row deletes, and the disk goes back at once. Being partitions
-- of the same tables, held messages stay in their destinations' queues (`deliveries_queue`) and
-- in the message browser; nothing that reads them changes.
--
-- The tables are created when first needed (`ductus_create_held_partition`, one table per
-- transaction), not here: creating a partition of `messages` locks it, and a migration holding
-- `messages` and `deliveries` together can deadlock with a running engine settling statuses.

-- The partition of `tab` (`messages` or `deliveries`) in the held area for the month holding
-- `day`, and the held table itself, created if missing; returns its name. Month bounds are at
-- midnight UTC (as tier partitions: 0010).
CREATE FUNCTION ductus_create_held_partition(tab text, day date) RETURNS text
LANGUAGE plpgsql AS $$
DECLARE
    first  date := date_trunc('month', day::timestamp)::date;
    parent text := tab || '_held';
    name   text := format('%s_held_%s', tab, to_char(first, 'YYYYMM'));
BEGIN
    IF tab NOT IN ('messages', 'deliveries') THEN
        RAISE EXCEPTION 'no held area for %', tab;
    END IF;
    IF to_regclass(parent) IS NULL THEN
        BEGIN
            EXECUTE format('CREATE TABLE %I PARTITION OF %I FOR VALUES IN (0) PARTITION BY RANGE (received_at)',
                parent, tab);
        EXCEPTION WHEN duplicate_table OR unique_violation THEN
            NULL; -- another session created it first
        END;
    END IF;
    IF to_regclass(name) IS NULL THEN
        BEGIN
            EXECUTE format('CREATE TABLE %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
                name, parent, first::timestamp AT TIME ZONE 'UTC', (first + interval '1 month') AT TIME ZONE 'UTC');
        EXCEPTION WHEN duplicate_table OR unique_violation THEN
            NULL;
        END;
    END IF;
    RETURN name;
END $$;

-- `quarantined: purge`: the interface's quarantined messages go with its expired partitions
-- instead of moving to the held area (`keep`, the default).
ALTER TABLE retention ADD COLUMN purge_quarantined boolean NOT NULL DEFAULT false;
