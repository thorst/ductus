-- The audit log (v0.5 step 8, part 2): who did what, to what, from where, and how it ended. It
-- records every sign-in, account and token change, payload read, message action, pause, drain and
-- retention decision, and the engine's own starts and stops. Not dropped with the messages'
-- partitions, and not tied to users or tokens by a foreign key: it must say who acted long after
-- the account is gone (the name is a snapshot).
CREATE TABLE audit_log (
    id         bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at         timestamptz NOT NULL DEFAULT now(),
    -- Who: 'user' (actor_id = users.id), 'token' (api_tokens.id), 'system' (the engine, or someone
    -- at the server's command line), 'anonymous' (a sign-in as a name nobody has).
    actor_kind text        NOT NULL CHECK (actor_kind IN ('user', 'token', 'system', 'anonymous')),
    actor_id   integer,
    -- The person behind it: the user, or the owner of a user's token. Their own activity.
    user_id    integer,
    actor_name text        NOT NULL,
    -- How they came: 'session' (the web UI), 'token' (a bearer token), 'cli', 'engine'. Rows made
    -- from message_actions before this table existed have none.
    via        text,
    ip         inet,
    -- What: message.payload.read, message.dismiss, user.create, ...
    action     text        NOT NULL,
    -- What it was done to: a username, a token name, `interface/destination`.
    subject    text,
    interface  text,
    message_id bigint,
    outcome    text        NOT NULL DEFAULT 'ok' CHECK (outcome IN ('ok', 'denied', 'failed')),
    detail     jsonb
);
CREATE INDEX audit_log_at ON audit_log (at);
CREATE INDEX audit_log_actor ON audit_log (actor_kind, actor_id, at);
CREATE INDEX audit_log_user ON audit_log (user_id, at) WHERE user_id IS NOT NULL;
CREATE INDEX audit_log_action ON audit_log (action, at);
CREATE INDEX audit_log_interface ON audit_log (interface, at) WHERE interface IS NOT NULL;
CREATE INDEX audit_log_message ON audit_log (message_id) WHERE message_id IS NOT NULL;

-- A dismissed quarantine joins resends and edits; each request's actions point at its audit row.
ALTER TABLE message_actions DROP CONSTRAINT message_actions_action_check;
ALTER TABLE message_actions
    ADD CONSTRAINT message_actions_action_check CHECK (action IN ('resend', 'edit', 'dismiss')),
    ADD COLUMN audit_id bigint;
CREATE INDEX message_actions_audit ON message_actions (audit_id) WHERE audit_id IS NOT NULL;
-- previous_output is a copy of a message's payload, so it is cleared when the message's own
-- retention ends (the row stays): the hourly job finds the ones to clear here.
CREATE INDEX message_actions_outputs ON message_actions (at) WHERE previous_output IS NOT NULL;

-- One audit row for each request that already made message_actions (a request made all its rows
-- in one transaction, so they share the time), so the timeline is one query.
DO $$
DECLARE
    g   record;
    aid bigint;
BEGIN
    FOR g IN
        SELECT at, action, actor, token_id, user_id, reason,
               array_agg(id ORDER BY id) AS ids,
               array_agg(DISTINCT message_id) AS messages,
               min(interface) AS interface,
               count(*) AS deliveries,
               COALESCE(bool_or(previous_status = 3), false) AS released
        FROM message_actions
        GROUP BY at, action, actor, token_id, user_id, reason
        ORDER BY min(id)
    LOOP
        INSERT INTO audit_log (at, actor_kind, actor_id, user_id, actor_name, via, action, interface, message_id, detail)
        VALUES (
            g.at,
            CASE WHEN g.token_id IS NOT NULL THEN 'token' WHEN g.user_id IS NOT NULL THEN 'user' ELSE 'system' END,
            COALESCE(g.token_id, g.user_id),
            g.user_id,
            g.actor,
            NULL,
            CASE WHEN g.action = 'edit' THEN 'message.edit'
                 WHEN g.released THEN 'message.release'
                 ELSE 'message.replay' END,
            g.interface,
            CASE WHEN cardinality(g.messages) = 1 THEN g.messages[1] END,
            jsonb_build_object('backfilled', true, 'messages', g.messages, 'deliveries', g.deliveries,
                               'reason', g.reason)
        )
        RETURNING id INTO aid;
        UPDATE message_actions SET audit_id = aid WHERE id = ANY(g.ids);
    END LOOP;
END $$;
