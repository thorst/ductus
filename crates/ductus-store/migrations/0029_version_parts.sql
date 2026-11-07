-- Upgrade: adds which files each destination's code loads to every code version, for history by source and by destination; nothing existing changes
-- Heavy: none
--
-- 0.7.4.2: `ductus history <interface> --destination lab` tells which destinations a change of
-- code touches by the files each destination's code loads. A version's files are known (`files`);
-- which of them each part runs is recorded from now on. Versions saved before have none until an
-- engine runs them again (it fills it in then): history says which of their changes it can't tell.
--
-- A column without a default: Postgres changes only its catalog, no row is rewritten.

-- `{"source": ["interfaces/a/answer.ts"], "destinations": {"lab": ["interfaces/a/lab.ts",
-- "lib/codes.ts"], "archive": []}}`: paths as in `files`; `source` holds an HTTP `respond`
-- interface's code (empty otherwise), each destination its code's files (empty: no code).
-- Null: saved before 0.7.4.2 and not run since.
ALTER TABLE code_versions ADD COLUMN parts jsonb;
