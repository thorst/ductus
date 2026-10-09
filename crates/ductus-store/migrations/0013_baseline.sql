-- What's normal for each destination, per hour of the week (v0.5 step 6b): messages arriving per
-- minute and the mean send time, 168 values each (Monday 00:00 local is the first). Monitoring
-- compares the last few minutes with them to tell a burst from a slow receiver. Written once an
-- hour, for the hour that just ended; NULL where an hour hasn't been watched yet.

ALTER TABLE destination_activity
    ADD COLUMN baseline_inflow  real[],
    ADD COLUMN baseline_send_ms real[];
