-- Two engines creating the same partition at once must not fail.
CREATE OR REPLACE FUNCTION ductus_ensure_partitions(days_ahead integer) RETURNS void
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
