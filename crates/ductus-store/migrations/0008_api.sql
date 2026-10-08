-- The REST API (v0.5 step 4): its tokens, and indexes for browsing messages and counting
-- quarantines without reading whole tables.

-- API tokens for scripts and tools. Only a SHA-256 hash of each token is kept (tokens are 32
-- random bytes, so a slow password hash adds nothing); the token itself is shown once, when
-- it's created. Revoked tokens stay for the audit trail.
CREATE TABLE api_tokens (
    id           integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name         text        NOT NULL,
    -- 0 viewer (read), 1 operator (drain, checks), 2 admin (tokens)
    role         smallint    NOT NULL,
    hash         bytea       NOT NULL UNIQUE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    revoked_at   timestamptz
);
-- One live token per name, so `revoke <name>` is unambiguous; a revoked name can be reused.
CREATE UNIQUE INDEX api_tokens_live_name ON api_tokens (name) WHERE revoked_at IS NULL;

-- The message browser: an interface's messages, newest first.
CREATE INDEX messages_by_interface ON messages (interface, id);
-- Quarantined messages are few and the ones people look for: their own small indexes.
CREATE INDEX messages_quarantined ON messages (interface, id) WHERE status = 3;
CREATE INDEX deliveries_quarantined ON deliveries (destination_id, message_id) WHERE status = 3;
