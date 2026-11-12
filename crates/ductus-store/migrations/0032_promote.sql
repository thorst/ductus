-- Upgrade: adds where releases were deployed on other databases (carried in their files), the branches each release was made on, and marks the deployments that were rollbacks; nothing existing changes
-- Heavy: none
--
-- 0.7.4.4.3: promotion and rollback. An environment may require a release to have been deployed
-- to others first (`DUCTUS_DEPLOY_REQUIRES=test`). Test and prod have separate databases, so the
-- record of where a release went travels in its file (the manifest's `deployed`, outside its id)
-- and is kept here when the file is stored, merged on every upload: a release stored before keeps
-- its first manifest, so the record can't live only there. A rollback's deployment says so, so the
-- next rollback skips the release it went back from.
--
-- A new table and a new nullable column without a default: Postgres changes only its catalog.

-- Where a release was deployed on another database, as its file said when it was stored here
-- (the deployments on this database are read from `deployments` itself). Unsigned: anyone who
-- holds the file can edit its manifest; said in the documentation.
CREATE TABLE release_places (
    release_id bytea NOT NULL,
    environment text NOT NULL,
    -- The engine's server and the deployment's id on that database.
    host text NOT NULL,
    deployment integer NOT NULL,
    at timestamptz NOT NULL,
    by_name text NOT NULL,
    ductus_version text NOT NULL,
    -- When and by whom the file carrying it was stored here.
    received_at timestamptz NOT NULL DEFAULT now(),
    received_by text NOT NULL,
    PRIMARY KEY (release_id, environment, host, deployment)
);

-- The deployment was a rollback (`ductus deploy --rollback`); null otherwise and before 0.7.4.4.3.
ALTER TABLE deployments ADD COLUMN rollback boolean;

-- Every branch the release was made on, as its files said (`git.branch`), merged at each upload:
-- the same files made on a feature branch and again on main after a fast-forward merge are one
-- release, and it was made on both. Null: none said, or stored before 0.7.4.4.3.
ALTER TABLE releases ADD COLUMN branches text[];
