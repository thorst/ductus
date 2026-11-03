//! What an isolate gives a debugger: inspector sessions opened from another thread (the
//! inspector server's), speaking the Chrome DevTools Protocol as text, and its guard's
//! [`DebugSwitch`](crate::DebugSwitch). The server itself is the runtime's (`ductus_runtime::inspect`).

use deno_core::futures::StreamExt;
use deno_core::futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use deno_core::{InspectorMsg, InspectorSessionChannels, InspectorSessionKind, InspectorSessionProxy};

/// Opens sessions on one isolate's inspector, from any thread.
#[derive(Clone)]
pub struct InspectorSessions(pub(crate) UnboundedSender<InspectorSessionProxy>);

impl InspectorSessions {
    /// A new session: what the isolate says, and where to send it messages. None once the
    /// isolate is gone.
    pub fn open(&self) -> Option<(FromIsolate, ToIsolate)> {
        let (to_client, from_isolate) = unbounded();
        let (to_isolate, from_client) = unbounded();
        let proxy = InspectorSessionProxy {
            channels: InspectorSessionChannels::Regular { tx: to_client, rx: from_client },
            // A debugger's session doesn't keep the program alive: it ends when the code does.
            kind: InspectorSessionKind::NonBlocking { wait_for_disconnect: false },
        };
        self.0.unbounded_send(proxy).ok()?;
        Some((FromIsolate(from_isolate), ToIsolate(to_isolate)))
    }
}

/// A session's messages from the isolate (answers and events), as protocol text.
pub struct FromIsolate(UnboundedReceiver<InspectorMsg>);

impl FromIsolate {
    /// The next one; None when the isolate is gone.
    pub async fn next(&mut self) -> Option<String> {
        self.0.next().await.map(|m| m.content)
    }
}

/// Where a session's messages to the isolate go. Dropping it ends the session.
pub struct ToIsolate(UnboundedSender<String>);

impl ToIsolate {
    /// False when the isolate is gone.
    pub fn send(&self, message: String) -> bool {
        self.0.unbounded_send(message).is_ok()
    }
}
