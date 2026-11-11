-- Upgrade: adds to each deployment the release it ran and its project version, and to each release check the Ductus version that ran it; nothing existing changes
-- Heavy: none
--
-- 0.7.4.4.1: an engine in release mode (`ductus run --releases`) runs the release last deployed to
-- its environment for its project: the deployments say which. A deploy holds a release against
-- what runs (the "replaces" check) by each interface's version and, for the files outside every
-- interface (`ductus.yaml`, `resources.yaml`, `environments/`, ...), by the project version; and
-- may reuse a recent passing check (the pre-run) only when the same Ductus version ran it.
--
-- New nullable columns without defaults: Postgres changes only its catalog (no table is rewritten).
-- The index is on `deployments`, a table written once per start: built at once.

-- The release this deployment ran (`ductus run --releases`); null in folder mode and before 0.7.4.4.
-- No foreign key: releases are kept (nothing deletes one an engine ran), and a key would make every
-- deployment's insert lock that row.
ALTER TABLE deployments ADD COLUMN release_id bytea;
-- The release's project version (SHA-256 over the files outside every interface); null in folder mode.
ALTER TABLE deployments ADD COLUMN project_version bytea;
-- The release last deployed to an environment: read at every start in release mode and by every deploy.
CREATE INDEX deployments_releases ON deployments (environment, id) WHERE release_id IS NOT NULL;

-- The Ductus version that ran the check (`ductus --version`); null before 0.7.4.4.
ALTER TABLE release_checks ADD COLUMN ductus_version text;
