-- Sessions for the web UI (v0.5 step 7): a browser signs in once with an API token and gets a
-- session cookie instead of keeping the token itself (page scripts can't read the cookie, and
-- it expires). A session acts with its token's role; revoking the token ends its sessions.
-- Step 8's user logins will make sessions the same way.
CREATE TABLE api_sessions (
    id           integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- SHA-256 of the cookie's value (32 random bytes, like tokens).
    hash         bytea       NOT NULL UNIQUE,
    token_id     integer     NOT NULL REFERENCES api_tokens (id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL
);
CREATE INDEX api_sessions_expires ON api_sessions (expires_at);
