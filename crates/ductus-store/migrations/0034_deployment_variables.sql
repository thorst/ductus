-- Upgrade: adds what each deployment's changed environment variables reached, per interface; nothing existing changes
-- Heavy: none
--
-- 0.7.5.2.3: a variable's value changed in place. An interface's code version is the hash of its
-- files as written (`${MLLP_PORT}`, not 6661): a release has no environment, and test and prod run
-- the same version. So when a deploy changes a variable's value, the interfaces it reaches (their
-- settings filled in differ, or their code names it) keep their version, and this table is how
-- their history tells it: a row per interface the deploy reached, with each variable's value
-- before and after. Variables aren't secrets (those are in secrets/, and a change there takes the
-- handover); their values are kept as the environment file has them.
--
-- A new table: Postgres changes only its catalog.

CREATE TABLE deployment_variables (
    deployment_id integer NOT NULL REFERENCES deployments (id) ON DELETE CASCADE,
    interface     text    NOT NULL,
    -- `[{"name": "MLLP_PORT", "before": "6661", "after": "6666"}]` by name; a null value: not set.
    variables     jsonb   NOT NULL,
    -- What they changed, in words: `source (MLLP_PORT)`, `code reads EMPI_URL`.
    what          text[]  NOT NULL,
    -- The parts they reach, as history narrows by part: `{"everything", "source", "destinations", "unsure"}`.
    touches       jsonb   NOT NULL,
    PRIMARY KEY (deployment_id, interface)
);
CREATE INDEX deployment_variables_interface ON deployment_variables (interface, deployment_id);
