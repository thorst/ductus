-- Upgrade: adds releases an engine composes (one interface out of a release, `ductus deploy R --interface A`) and which interfaces of a release went where when only some did; nothing existing changes
-- Heavy: none
--
-- 0.7.5.3.2: composed releases. `ductus deploy R --interface A` deploys only A's work from release
-- R: the engine stores a release of its own (what ran, with A's files from R) and deploys that.
-- Its deployments record R's commit (where A came from), so the "replaces" check's "a later
-- deployment ran this version from a commit in the history" must not read them as vouching for
-- the interfaces it didn't take; and an environment requiring another first
-- (`DUCTUS_DEPLOY_REQUIRES`) takes A from R once a release composed from R took A there.
--
-- Nullable columns without defaults: Postgres changes only its catalog.

-- The release a composed release took interfaces from, and which; null for a release made by
-- `ductus release`.
ALTER TABLE releases ADD COLUMN composed_from bytea;
ALTER TABLE releases ADD COLUMN composed_interfaces text[];

-- Only these interfaces of the release went there (a release composed from it, on the database
-- the file came from); null: the whole release.
ALTER TABLE release_places ADD COLUMN interfaces text[];
