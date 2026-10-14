-- Alerts (v0.5 step 8, part 3). One row per thing that can alert (an interface, a destination),
-- made when the engine with `alerts:` first runs it, so it can be silenced before anything fires.
-- Changes are compare-and-set on `rev`: two engines on one database judge the same state and only
-- one of them records each change.
CREATE TABLE alert_state (
    id               integer     GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- 'iface:<interface>' or 'dest:<interface>/<destination>'.
    key              text        NOT NULL UNIQUE,
    interface        text        NOT NULL,
    destination      text,
    state            text        NOT NULL DEFAULT 'ok'
                     CHECK (state IN ('ok', 'error', 'stalled', 'behind', 'stopped', 'silent', 'missed')),
    severity         text        CHECK (severity IN ('warning', 'critical')),
    -- Bad since; fine again since (not yet for `alerts.settle`).
    since            timestamptz,
    ok_since         timestamptz,
    -- When people were last told, or will be once quiet hours end; notifications sent since it fired.
    last_notified_at timestamptz,
    notify_count     integer     NOT NULL DEFAULT 0,
    summary          text,
    ack_by           text,
    ack_at           timestamptz,
    silenced_until   timestamptz,
    silenced_by      text,
    silence_reason   text,
    rev              bigint      NOT NULL DEFAULT 0,
    updated_at       timestamptz NOT NULL DEFAULT now()
);

-- What happened to each alert, kept `alerts.keep` (a year by default).
CREATE TABLE alert_events (
    id         bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at         timestamptz NOT NULL DEFAULT now(),
    alert_id   integer     NOT NULL,
    key        text        NOT NULL,
    kind       text        NOT NULL
               CHECK (kind IN ('fired', 'escalated', 'reminded', 'recovered', 'acknowledged', 'silenced', 'unsilenced')),
    state      text,
    severity   text,
    -- When the alert turned bad, for the words of a notification sent later.
    since      timestamptz,
    summary    text,
    -- Who acknowledged or silenced it.
    by_name    text,
    detail     jsonb
);
CREATE INDEX alert_events_alert ON alert_events (alert_id, id);
CREATE INDEX alert_events_at ON alert_events (at);

-- The outbox: a notification is stored with its event before anything is sent, then sent (and
-- retried) by whichever engine claims it first (FOR UPDATE SKIP LOCKED).
CREATE TABLE alert_deliveries (
    id          bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id    bigint      NOT NULL REFERENCES alert_events (id) ON DELETE CASCADE,
    -- A channel's name, or 'email:<address>' for an owner.
    channel     text        NOT NULL,
    status      text        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed', 'cancelled')),
    attempts    integer     NOT NULL DEFAULT 0,
    last_error  text,
    -- Due then (after quiet hours, or a back-off); while claimed, the end of the claim.
    next_try_at timestamptz NOT NULL DEFAULT now(),
    sent_at     timestamptz
);
CREATE INDEX alert_deliveries_due ON alert_deliveries (next_try_at) WHERE status = 'pending';
CREATE INDEX alert_deliveries_event ON alert_deliveries (event_id);
