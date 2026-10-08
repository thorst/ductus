//! Whether an interface takes new messages. Drain mode closes every interface's gate: sources
//! stop receiving (MLLP listeners close, queued HTTP answers 503, file pickup and schedules
//! pause) while workers keep delivering what's already stored.
//!
//! "Drained" needs more than a closed gate: a message accepted just before closing may still be
//! committing. Every store goes through a `Pass`, counted while it's in flight, so once the gate
//! is closed and nothing is in flight, no new message can appear.

use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::watch;

#[derive(Debug)]
pub struct Gate {
    open: watch::Sender<bool>,
    in_flight: AtomicUsize,
}

impl Default for Gate {
    fn default() -> Self {
        Self { open: watch::Sender::new(true), in_flight: AtomicUsize::new(0) }
    }
}

/// Error for a message offered while the gate is closed: nothing was stored, so the sender must
/// try again later (it isn't the sender's fault, and nothing is lost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not taking new messages (the engine is draining)")
    }
}

impl std::error::Error for Closed {}

/// Held while a message is being stored; see `Gate::enter`.
pub struct Pass<'a>(&'a Gate);

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Gate {
    pub fn is_open(&self) -> bool {
        *self.open.borrow()
    }

    pub fn set_open(&self, open: bool) {
        self.open.send_if_modified(|o| std::mem::replace(o, open) != open);
    }

    /// Wait until the gate is open (at once if it is).
    pub async fn opened(&self) {
        let _ = self.open.subscribe().wait_for(|o| *o).await;
    }

    /// Wait until the gate is closed (at once if it is).
    pub async fn closed(&self) {
        let _ = self.open.subscribe().wait_for(|o| !*o).await;
    }

    /// Permission to store a message, held until it's committed (or failed). Counted first and
    /// checked second: a closer that sets the gate and then sees nothing in flight can be sure
    /// every later `enter` sees the gate closed.
    pub fn enter(&self) -> Result<Pass<'_>, Closed> {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let pass = Pass(self);
        if self.is_open() { Ok(pass) } else { Err(Closed) }
    }

    /// Messages being stored right now.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn counts_passes_and_refuses_when_closed() {
        let gate = Gate::default();
        let pass = gate.enter().unwrap();
        assert_eq!(gate.in_flight(), 1);
        gate.set_open(false);
        assert_eq!(gate.enter().err(), Some(Closed));
        assert_eq!(gate.in_flight(), 1, "a refused pass isn't counted");
        drop(pass);
        assert_eq!(gate.in_flight(), 0);

        let gate = std::sync::Arc::new(gate);
        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move { gate.opened().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        gate.set_open(true);
        tokio::time::timeout(Duration::from_secs(1), waiter).await.unwrap().unwrap();
        assert!(gate.enter().is_ok());
    }
}
