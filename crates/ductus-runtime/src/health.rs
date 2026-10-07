//! What the engine knows about each running interface beyond its queue: whether it has been
//! stopped, and why. Shared by the interface's worker slots (and, later, the admin API).

use ductus_script::{Cause, Runaway};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

/// Runaway timeouts in a row that stop an interface. One could be a pathological message
/// (quarantined, and the rest flow); three in a row is broken code.
const RUNAWAYS_TO_STOP: u32 = 3;

#[derive(Debug, Default)]
pub struct Health {
    stopped: OnceLock<String>,
    runaways_in_a_row: AtomicU32,
}

impl Health {
    /// Why the interface was stopped, if it was. Its messages wait (pending) until the code is
    /// fixed and the engine restarted.
    pub fn stopped(&self) -> Option<&str> {
        self.stopped.get().map(String::as_str)
    }

    /// The interface's code ran away. Returns the reason if this stops the interface: a heap
    /// overflow always does (what it holds on to can't be freed while its code is loaded),
    /// timeouts after `RUNAWAYS_TO_STOP` in a row.
    pub fn runaway(&self, runaway: &Runaway) -> Option<&str> {
        let in_a_row = self.runaways_in_a_row.fetch_add(1, Ordering::Relaxed) + 1;
        let reason = match runaway.cause {
            Cause::Heap(_) => format!("{runaway}"),
            Cause::Timeout(_) if in_a_row >= RUNAWAYS_TO_STOP => {
                format!("{in_a_row} runaway scripts in a row; last: {runaway}")
            }
            Cause::Timeout(_) => return None,
        };
        self.stopped.set(reason).ok()?;
        self.stopped()
    }

    /// The code ran normally (whatever it decided about the message).
    pub fn ran(&self) {
        if self.runaways_in_a_row.load(Ordering::Relaxed) != 0 {
            self.runaways_in_a_row.store(0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stops_after_three_timeouts_in_a_row_or_one_heap_overflow() {
        let timeout = Runaway { interface: None, cause: Cause::Timeout(Duration::from_secs(5)) };
        let health = Health::default();
        assert!(health.runaway(&timeout).is_none());
        assert!(health.runaway(&timeout).is_none());
        health.ran(); // a normal run resets the count
        assert!(health.runaway(&timeout).is_none());
        assert!(health.runaway(&timeout).is_none());
        let reason = health.runaway(&timeout).unwrap().to_string();
        assert!(reason.starts_with("3 runaway scripts in a row") && reason.contains("5s timeout"), "{reason}");
        assert_eq!(health.stopped(), Some(reason.as_str()));
        assert!(health.runaway(&timeout).is_none(), "reported once");

        let heap = Health::default();
        let reason = heap.runaway(&Runaway { interface: None, cause: Cause::Heap(512) }).unwrap();
        assert!(reason.contains("512 MB"), "{reason}");
    }
}
