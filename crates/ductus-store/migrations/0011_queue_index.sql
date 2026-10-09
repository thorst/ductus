-- Each destination's queue in message order (v0.5 step 6). The worker reads a destination's
-- oldest 50 pending deliveries in its lanes. With the queue indexed by lane first, Postgres
-- had to collect and sort a lane set's whole backlog, so it walked each partition's primary
-- key in message order instead, skipping every finished delivery older than the backlog: with
-- 1M delivered messages ahead of a backlog of 10k, a read took ~300 ms instead of ~1.5 ms (a
-- destination back from an outage drained at ~170 messages/s). In message order, the read
-- stops after 50 rows; the lane is checked inside the index.
--
-- Built in place on upgrade: writes to `deliveries` wait while it scans the table (only pending
-- rows go into it). Fine before release; online index builds are a v1.0 upgrade item.
CREATE INDEX deliveries_queue ON deliveries (destination_id, message_id, partition_key) WHERE status = 0;
-- Everything that used the lane-first index filters by destination, which this one serves.
DROP INDEX deliveries_pending;
