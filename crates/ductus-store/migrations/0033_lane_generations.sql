-- Upgrade: adds the lanes each interface's messages were stored in, so changing `ordering` never strands or reorders messages; fills it from the lane count each interface last ran with
-- Heavy: none
--
-- 0.7.4.4.4: changing lanes made easy. A message's lane is worked out once, at intake, from its
-- ordering key and the interface's lane count. Changing the count (or the key) used to need an
-- empty queue: lowered, messages waited in lanes no worker read; raised, a key's new messages
-- could pass its older ones. Now a change adds a generation: lanes numbered after the last
-- generation's, where new messages are stored. Workers read every generation listed; a lane is
-- held while older lanes that may hold the same keys still have messages for that destination,
-- and a generation is deleted once nothing waits in it. Lanes are plain numbers to the queue, so
-- its rows, indexes and reads are unchanged.
--
-- `interface_applied` keeps being written (the end of the newest generation's lanes), so an older
-- binary that would read lanes below that sees a lowering and checks what waits above its own.

CREATE TABLE lane_generations (
    interface  text        NOT NULL,
    -- Its lanes are first_lane .. first_lane + lanes - 1; the generations of an interface never
    -- overlap, and a newer one always starts after an older one ends.
    first_lane integer     NOT NULL CHECK (first_lane >= 0),
    lanes      integer     NOT NULL CHECK (lanes >= 1),
    -- `ordering.key` as configured ('' without one). NULL: not known (filled from
    -- `interface_applied` here, and set by the next start without making a new generation).
    key        text,
    -- Its messages' lanes don't follow the key (an older binary's, found at a start): every newer
    -- lane waits for all of it.
    mixed      boolean     NOT NULL DEFAULT false,
    since      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (interface, first_lane)
);

INSERT INTO lane_generations (interface, first_lane, lanes)
SELECT interface, 0, lanes FROM interface_applied;
