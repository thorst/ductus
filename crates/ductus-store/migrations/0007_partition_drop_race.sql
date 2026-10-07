-- A `messages` partition dropped while delivery partitions are being made (retention drops old
-- days; found by the store stress test, where another test's cleanup did) has no bounds any
-- more: `pg_get_expr` returns NULL and the CREATE statement came out incomplete ("syntax error
-- at end of input"). Skip partitions that disappear mid-scan; a dropped day needs no deliveries.
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
        CONTINUE WHEN p.bounds IS NULL;
        BEGIN
            EXECUTE format('CREATE TABLE IF NOT EXISTS %I PARTITION OF deliveries %s', p.name, p.bounds);
        EXCEPTION WHEN duplicate_table OR unique_violation THEN
            NULL;
        END;
    END LOOP;
END $$;
