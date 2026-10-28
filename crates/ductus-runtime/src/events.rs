//! Log lines the Windows service also writes to the Event Log (`ductus-cli`'s `service::eventlog`
//! picks them out by message, each with its own event ID): named here so a reworded line can't
//! silently stop reaching it.

/// The engine is up (every interface and the API).
pub const STARTED: &str = "started";
/// A stop finished what was in hand.
pub const STOPPED: &str = "stopped";
/// A stop gave up waiting for the messages in hand (its message goes on with the limit).
pub const STOP_TIMED_OUT: &str = "still finishing after";
/// A second stop request cut the stop short.
pub const STOP_CUT_SHORT: &str =
    "asked again: stopping now; the messages in hand are processed again at the next start";
/// How the last run on this host ended, when it didn't stop cleanly.
pub const PREVIOUS_RUN_UNCLEAN: &str = "the last run of the engine on this host did not stop cleanly (it was killed, crashed or lost power); messages in hand are processed again, as the delivery guarantee says";
/// The runaway guard stopped an interface.
pub const INTERFACE_STOPPED: &str =
    "interface stopped: its messages wait until the code is fixed and the engine restarted";
/// An alert changed (fields `kind`: fired, escalated, …, and `severity`).
pub const ALERT: &str = "alert";
