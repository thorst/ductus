-- Certificates and the engine's own connection checks (v0.5 step 8, part 4).

-- The certificate each TLS server the engine connects to presented at its last check. A check
-- that couldn't read one (refused, timed out) sets `error` and `checked_at` and keeps the dates
-- from the check before: the certificate out there still expires when it did.
CREATE TABLE certificate_status (
    -- `empi` (resources.yaml), `alerts.smtp`, `alerts.<channel>`, `alerts.heartbeat`, or
    -- `<interface>/<destination>` for a destination with a URL of its own.
    resource            text        NOT NULL,
    host                text        NOT NULL,
    port                integer     NOT NULL,
    -- The server's own certificate.
    subject             text,
    issuer              text,
    -- SHA-256 of the server certificate (DER), hex: changes when it is renewed or replaced.
    fingerprint         text,
    not_before          timestamptz,
    not_after           timestamptz,
    -- The earliest expiry in the chain the server presents (an intermediate can end first), and
    -- whose it is when it isn't the server certificate's.
    chain_min_not_after timestamptz,
    chain_min_subject   text,
    -- Whether this server trusts it (the system's trusted certificates); why not, if not.
    trusted             boolean,
    -- Why the last check failed (couldn't connect, not trusted, expired); none if it passed.
    error               text,
    checked_at          timestamptz NOT NULL,
    -- When the certificate above was read (the last check, unless that one couldn't read it).
    read_at             timestamptz,
    PRIMARY KEY (resource, host, port)
);

-- Each connection check the engine ran (on its schedule) or was asked for (`POST /doctor`):
-- the whole report. The last 30 are kept.
CREATE TABLE doctor_runs (
    id         bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at         timestamptz NOT NULL DEFAULT now(),
    -- 'certificates' or 'full'.
    mode       text        NOT NULL,
    -- 'schedule', or who asked (a user or a token).
    started_by text        NOT NULL,
    seconds    double precision NOT NULL,
    checks     integer     NOT NULL,
    failed     integer     NOT NULL,
    report     jsonb       NOT NULL
);

-- A certificate's alert: `certificate` is a state, and `stage` is the step of its countdown it
-- was last told about (one of `alerts.certificates.warn_days`, 0 once expired). Older engines
-- don't judge certificate alerts (their keys aren't theirs) and never write `stage`.
ALTER TABLE alert_state DROP CONSTRAINT alert_state_state_check;
ALTER TABLE alert_state ADD CONSTRAINT alert_state_state_check
    CHECK (state IN ('ok', 'error', 'stalled', 'behind', 'stopped', 'silent', 'missed', 'certificate'));
ALTER TABLE alert_state ADD COLUMN stage integer;
