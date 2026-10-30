-- Upgrade: keeps the names accounts had before, and the directory's id for each directory account
-- Heavy: none
--
-- 0.6.7.1: a directory account is the directory's person, not the name they typed. Its id
-- (entryUUID, or Active Directory's objectGUID) follows them through a rename, and tells a new
-- person given a departed one's sign-in name apart from them.

-- `<attribute>:<value>` (`entryUUID:6f1c...`, `objectGUID:hex:00ff...`); set at the first sign-in
-- or refresh that reads it. Null for local accounts, and directory ones not checked since 0.6.7.1.
ALTER TABLE users ADD COLUMN directory_id text;
CREATE UNIQUE INDEX users_directory_id ON users (directory_id) WHERE directory_id IS NOT NULL;

-- What an account was called before: every change of its username, display name or email keeps
-- the old value here, written with the change.
CREATE TABLE user_names (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id integer NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('username', 'email', 'display_name')),
    value text NOT NULL,
    until timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX user_names_value ON user_names (lower(value));
CREATE INDEX user_names_user ON user_names (user_id);
