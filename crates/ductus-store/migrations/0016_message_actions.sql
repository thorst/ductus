-- What people did to messages (v0.5 step 7): resending a delivery (it goes back to pending in
-- place, keeping the message's id and lane) and editing a message (a corrected copy is queued,
-- linked here to the original). One row per message and destination acted on, with what the
-- delivery was before, so nothing an action changes is lost. Step 8's audit log builds on it.
-- Not dropped with the messages' partitions: it's the record of who did what.
CREATE TABLE message_actions (
    id                bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    at                timestamptz NOT NULL DEFAULT now(),
    -- 'resend' or 'edit'
    action            text        NOT NULL CHECK (action IN ('resend', 'edit')),
    -- Who: the API token (and its name then: names can be reused once revoked). Users come with step 8.
    token_id          integer     REFERENCES api_tokens (id),
    actor             text        NOT NULL,
    reason            text,
    message_id        bigint      NOT NULL,
    received_at       timestamptz NOT NULL,
    interface         text        NOT NULL,
    destination_id    integer     NOT NULL REFERENCES destinations (id),
    -- The delivery before the action (none: the message had no delivery for that destination).
    previous_status   smallint,
    previous_attempts integer,
    previous_error    text,
    -- What was sent before, for destinations with `save_output: true` (sending again replaces it).
    previous_output   bytea,
    -- 'edit': the corrected copy.
    copy_id           bigint
);
CREATE INDEX message_actions_message ON message_actions (message_id);
CREATE INDEX message_actions_copy ON message_actions (copy_id) WHERE copy_id IS NOT NULL;
