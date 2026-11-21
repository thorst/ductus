-- Upgrade: lets an interface's alert record the new `keyless` state (its keys read empty); nothing existing changes
-- Heavy: none
--
-- 0.7.7.1: an empty key is never silent. `keyless` joins the states an alert can be in. An older
-- engine reading it back sees a state it doesn't know and judges the interface afresh.
ALTER TABLE alert_state DROP CONSTRAINT alert_state_state_check;
ALTER TABLE alert_state ADD CONSTRAINT alert_state_state_check
    CHECK (state IN ('ok', 'error', 'stalled', 'behind', 'stopped', 'silent', 'missed', 'certificate', 'keyless'));
