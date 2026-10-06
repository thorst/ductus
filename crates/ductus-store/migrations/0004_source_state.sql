-- What sources remember between polls and restarts: how far into a file they've read, when a
-- schedule last ran. Always written in the same transaction as the messages it accounts for,
-- so after a crash the state and the stored messages agree (no gaps, no duplicates).
CREATE TABLE source_state (
    interface  text        NOT NULL,
    key        text        NOT NULL,
    value      jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (interface, key)
);
