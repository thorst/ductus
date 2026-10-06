-- Keys of recently received messages, per interface, for duplicate detection.
-- Status 4 = duplicate (stored for audit, never delivered).
CREATE TABLE dedupe_keys (
    interface text        NOT NULL,
    key       text        NOT NULL,
    seen_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (interface, key)
);

CREATE INDEX dedupe_keys_seen_at ON dedupe_keys (seen_at);
