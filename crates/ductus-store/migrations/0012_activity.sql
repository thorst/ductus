-- Monitoring that survives restarts and failover (v0.5 step 6b): when each interface last received
-- something and each destination last sent or failed, and destinations paused on purpose. A
-- restart or a power blip mustn't reset "last activity", or "nothing received for 30 minutes"
-- would start counting again. One task writes the times every few seconds (a crash loses at most
-- those seconds); pauses are written at once.

CREATE TABLE interface_activity (
    interface     text PRIMARY KEY,
    -- When an engine first ran the interface: deadlines before it don't count.
    watched_since timestamptz NOT NULL DEFAULT now(),
    last_received timestamptz,
    updated_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE destination_activity (
    destination_id integer PRIMARY KEY REFERENCES destinations (id),
    last_sent      timestamptz,
    last_error     text,
    last_error_at  timestamptz,
    -- Paused on purpose (`ductus pause`): nothing is sent or retried until it's resumed.
    paused_at      timestamptz,
    paused_by      text,
    -- Optional, and often added after the pause (act first, document after).
    pause_reason   text,
    reason_by      text,
    reason_at      timestamptz,
    updated_at     timestamptz NOT NULL DEFAULT now()
);
