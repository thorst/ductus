-- People (v0.5 step 8, part 1): user accounts with a role and permissions, their sessions, their
-- own tokens, and their settings (appearance). Before this, the only identity was an API token.

CREATE TABLE users (
    id                   integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    username             text        NOT NULL,
    display_name         text        NOT NULL DEFAULT '',
    email                text        NOT NULL DEFAULT '',
    -- Same numbers as api_tokens: 0 viewer, 1 operator, 2 admin.
    role                 smallint    NOT NULL CHECK (role BETWEEN 0 AND 2),
    -- Extra things a role doesn't say: 'payloads' (may read message payloads), 'audit' (may read
    -- the audit log). Admins have both.
    permissions          text[]      NOT NULL DEFAULT '{}',
    -- Where the password is checked: 'local' (password_hash) or a directory ('ldap', later).
    source               text        NOT NULL DEFAULT 'local' CHECK (source IN ('local', 'ldap')),
    -- argon2id, as a PHC string; null for accounts whose password lives elsewhere.
    password_hash        text,
    must_change_password boolean     NOT NULL DEFAULT false,
    disabled_at          timestamptz,
    failed_logins        integer     NOT NULL DEFAULT 0,
    last_failed_at       timestamptz,
    locked_until         timestamptz,
    created_at           timestamptz NOT NULL DEFAULT now(),
    last_sign_in_at      timestamptz
);
-- Usernames are not case sensitive ("Anna" and "anna" are one person).
CREATE UNIQUE INDEX users_username ON users (lower(username));

-- A user's own choices, kept on the server so they follow them to any machine.
CREATE TABLE user_settings (
    user_id    integer     PRIMARY KEY REFERENCES users (id),
    theme      text        NOT NULL DEFAULT 'system',
    -- The five custom colors and the values worked out from them, as the UI saves them.
    colors     jsonb,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Tokens may belong to a user (they stop working when the user is disabled, and never go above
-- the user's role or permissions); without an owner they are service tokens, made by an admin.
ALTER TABLE api_tokens
    ADD COLUMN user_id     integer REFERENCES users (id),
    ADD COLUMN permissions text[]  NOT NULL DEFAULT '{}';
CREATE INDEX api_tokens_user ON api_tokens (user_id) WHERE user_id IS NOT NULL;

-- Nothing is installed that depends on the tokens made before accounts existed, and they could
-- read payloads without anyone saying so: every one of them is revoked (new ones are made with the
-- permissions they need). Their sessions go with them.
UPDATE api_tokens SET revoked_at = now() WHERE revoked_at IS NULL;
DELETE FROM api_sessions;

-- A session belongs to a user (signed in with a password) or to a token (as before).
ALTER TABLE api_sessions
    ALTER COLUMN token_id DROP NOT NULL,
    ADD COLUMN user_id integer REFERENCES users (id),
    ADD CONSTRAINT api_sessions_one_owner CHECK ((token_id IS NULL) <> (user_id IS NULL));
CREATE INDEX api_sessions_user ON api_sessions (user_id) WHERE user_id IS NOT NULL;

-- Who acted, when it was a person.
ALTER TABLE message_actions ADD COLUMN user_id integer REFERENCES users (id);
