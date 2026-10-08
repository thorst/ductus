-- Internal routing (v0.5): a destination can put messages straight into another interface's
-- queue. The message stored there remembers the message it came from.

-- The id of the message (in the sending interface) this one was routed from; NULL for messages
-- received from outside. Nullable without a default: adding it rewrites nothing.
ALTER TABLE messages ADD COLUMN origin_id bigint;
