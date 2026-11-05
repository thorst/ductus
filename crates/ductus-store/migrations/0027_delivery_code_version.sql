-- Upgrade: adds a column to deliveries and to message_actions naming the code version that decided each delivery; waits at most 2 s for each table's lock, else the start fails saying so and changes nothing
-- Heavy: none
--
-- 0.7.3.2: every delivery says which code decided it (`code_versions`, migration 0026), and a person
-- sending one again keeps what decided it before in the action's row.
--
-- Adding a column without a default is a change to the catalog only (no row is rewritten: ~5 ms
-- with 70 partitions, measured), but it needs each table's lock to itself, and it waits for every
-- query already reading the table. While it waits, every later statement on the table waits
-- behind it: inserts too, so the senders' ACKs. So it waits at most 2 s, then fails (a start says
-- why; a handover leaves the old engine serving), and senders wait those 2 s at most, never refused.
-- `message_actions` first: while the second ALTER waits, the first table's lock is held, and a
-- person's resend waiting is better than every sender waiting.
SET LOCAL lock_timeout = '2s';

-- A resend's row: the code version that decided the delivery before it was sent again (null: none
-- did, or an engine before 0.7.3.2).
ALTER TABLE message_actions ADD COLUMN previous_code_version integer;

-- The code version (`code_versions.id`) that decided this delivery: set with its final status by
-- the engine's workers. Null while pending, when a person decided it (dismissed, replaced by an
-- edited copy), when the engine did without running code (a destination removed, an older
-- engine's status taken over), or decided by an engine before 0.7.3.2. No foreign key: checking
-- one would cost a lookup per delivery decided, and versions are never deleted.
ALTER TABLE deliveries ADD COLUMN code_version integer;
