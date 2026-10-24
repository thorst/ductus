-- Our certificates (v0.6, step 3, part 3): the `certificates:` pairs the engine presents (to
-- senders on its TLS listeners, or as a client certificate) join the certificates it connects to,
-- so their expiry alerts the same way. Read from what the engine loaded, with no network: one row
-- per pair, `host` = the engine's server, `port` = 0 (it may be served on several ports). Older
-- engines insert without `side`: their rows are the remote ones they always were.
ALTER TABLE certificate_status
    ADD COLUMN side text NOT NULL DEFAULT 'remote' CHECK (side IN ('remote', 'ours'));
