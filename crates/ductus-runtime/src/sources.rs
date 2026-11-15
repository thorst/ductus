//! Each interface's source (what it receives with: an MLLP listener, file or SFTP pickup, a
//! schedule, an HTTP route), started at the engine's start, or alone when a deploy in place adds
//! the interface (0.7.5.2.1), changed alone when one changes it (0.7.5.2.2), and stopped alone
//! when one removes it: the other interfaces' sources never notice.
//!
//! Each MLLP listener, file pickup and schedule has a stop of its own (a child of the engine's
//! run). HTTP interfaces sharing a port share one server, whose router is swapped whole when a
//! route is added, removed or changed (requests in flight finish on the router they came to); a
//! server is started for an address no route had, and stopped once it routes to nothing.
//!
//! A source changed in place (`change`):
//! - **The same MLLP address** (only `tls:` or `allow:` changed): swapped in the running listener
//!   (`accepting::Accepting::swap`): new connections use the new ones; open connections keep their
//!   TLS session, and those the new `allow:` refuses are closed after the message in hand.
//! - **A moved listener** (another port or host): the new one listens first, then the old one
//!   closes as a drain closes it (each connection's message in hand stored and ACKed; senders
//!   reconnect to the new port).
//! - **An HTTP route** (path, method, port, `tls:`, `allow:`): rerouted on its server, or moved to
//!   another server (which listens first).
//! - **File, SFTP, schedule**: the old one stopped first, as a stop stops it, then the new one
//!   started: two pickups of one folder must never run at once.
//!
//! What only the handover can do (`in_place`): a port moving from one interface to another (or
//! an HTTP method and path), a listener changing kind or host on its own port, and an interface
//! changing between queue and respond mode. The handover keeps every socket open, so no sender is
//! refused there.
//!
//! Stopping one: its listening socket is given back first (`systemd::release`: neither systemd
//! nor this process keeps it, so the port is free), then the listener closes the way a drain
//! closes it: MLLP connections finish the message in hand (stored and ACKed) and close; an HTTP
//! server answers the requests in flight; file pickup and schedules stop between messages.

use crate::http::{Endpoint, Listen, Route, Server};
use crate::in_place::NeedsHandover;
use crate::ingest::Intake;
use anyhow::{Context, Result};
use ductus_core::InterfaceConfig;
use ductus_core::config::Source;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long a source stopping alone may take to finish what it has in hand (as a handover waits
/// for HTTP servers): past it, it is left to end by itself.
const STOP_WITHIN: Duration = Duration::from_secs(5);

/// HTTP/1.1, what HTTP sources speak over TLS.
pub(crate) const HTTP1: &[&[u8]] = &[b"http/1.1"];

/// The sources running, and what starting one more takes.
pub struct Sources {
    names: Arc<crate::allow::Names>,
    certificates: Arc<crate::tls::Certificates>,
    sftp: BTreeMap<String, ductus_core::resources::SftpServer>,
    ownership: Arc<crate::ownership::Ownership>,
    /// The engine's run: every MLLP listener, file pickup and schedule stops with it.
    ended: CancellationToken,
    /// HTTP servers stop with it (first in a handover: a request in flight is answered here, the
    /// next one by the new engine).
    receiving: CancellationToken,
    /// Each MLLP listener, file pickup and schedule, by interface.
    started: Mutex<HashMap<String, Started>>,
    /// Each HTTP server, with its routes.
    servers: Mutex<Vec<Server>>,
    /// Held while routes are added, moved or removed, so two deploys' interfaces on one new
    /// address don't both start a server for it.
    routing: tokio::sync::Mutex<()>,
}

/// One interface's MLLP listener, file pickup or schedule.
struct Started {
    stop: CancellationToken,
    task: JoinHandle<()>,
    /// MLLP: where it listens, and what it accepts with (swapped in place).
    listens: Option<Listening>,
}

struct Listening {
    host: String,
    port: u16,
    accepting: Arc<crate::accepting::Accepting>,
}

/// How the log names an interface's MLLP listener.
fn mllp_label(name: &str) -> String {
    format!("interface `{name}` (MLLP)")
}

impl Sources {
    pub(crate) fn new(
        names: Arc<crate::allow::Names>,
        certificates: Arc<crate::tls::Certificates>,
        sftp: BTreeMap<String, ductus_core::resources::SftpServer>,
        ownership: Arc<crate::ownership::Ownership>,
        ended: CancellationToken,
        receiving: CancellationToken,
    ) -> Self {
        Self {
            names,
            certificates,
            sftp,
            ownership,
            ended,
            receiving,
            started: Default::default(),
            servers: Default::default(),
            routing: Default::default(),
        }
    }

    /// Who may connect to a listener (`label` names it in the log), from its `allow:` list.
    pub(crate) fn admission(
        &self,
        label: &str,
        host: &str,
        port: u16,
        allow: Option<&ductus_core::allow::Allow>,
    ) -> Result<crate::tls::Admission> {
        allow.map(|a| crate::allow::Admission::new(label, host, port, a, self.names.clone())).transpose()
    }

    /// A listener's TLS (`tls:`), speaking `alpn`.
    pub(crate) fn server_tls(
        &self,
        tls: &Option<ductus_core::tls::ListenTls>,
        alpn: &[&[u8]],
    ) -> Result<Option<Arc<rustls::ServerConfig>>> {
        tls.as_ref().map(|t| self.certificates.server(t, alpn)).transpose()
    }

    /// Start `config`'s MLLP listener, file pickup or schedule, feeding `intake` (an HTTP source
    /// is served with the others on its port: `serve`, `route`; an internal one has none). The
    /// first bind is part of it: a port in use is an error.
    pub(crate) async fn start(&self, config: &InterfaceConfig, intake: Option<Arc<Intake>>) -> Result<()> {
        let name = &config.name;
        let intake = || intake.clone().with_context(|| format!("interface `{name}` has no intake"));
        let stop = self.ended.child_token();
        let (task, listens) = match &config.source {
            Source::Mllp { host, port, tls, allow } => {
                let label = mllp_label(name);
                let admission = self.admission(&label, host, *port, allow.as_ref())?;
                let accepting = crate::accepting::Accepting::new(&label, self.server_tls(tls, &[])?, admission);
                let address = (host.clone(), *port);
                let task = crate::ingest::spawn_mllp(intake()?, address, accepting.clone(), stop.clone()).await?;
                (task, Some(Listening { host: host.clone(), port: *port, accepting }))
            }
            Source::File(_) | Source::Sftp(_) => {
                let server = match &config.source {
                    Source::Sftp(f) => f.server.as_deref().and_then(|s| self.sftp.get(s).cloned()),
                    _ => None,
                };
                let watcher = crate::files::FileWatcher::new(config, intake()?, server, self.ownership.clone())?;
                (until(&stop, watcher.run()), None)
            }
            Source::Schedule(s) => {
                let timing = ductus_core::schedule::Timing::new(s)?;
                (until(&stop, crate::schedule::run(intake()?, timing, self.ownership.clone())), None)
            }
            Source::Http { .. } | Source::Internal => return Ok(()),
        };
        self.started.lock().unwrap_or_else(|e| e.into_inner()).insert(name.clone(), Started { stop, task, listens });
        Ok(())
    }

    /// Serve `endpoints`: one server per address, with every route on it (`http::serve`).
    pub(crate) async fn serve(&self, endpoints: Vec<Endpoint>) -> Result<()> {
        let servers = crate::http::serve(endpoints, &self.receiving).await?;
        self.servers.lock().unwrap_or_else(|e| e.into_inner()).extend(servers);
        Ok(())
    }

    /// Start the source of `config`'s interface, which a deploy in place adds (`in_place` let it):
    /// host names in its `allow:` looked up first, then as `start`; an HTTP one routed on the
    /// server for its address (started if none), storing into `intake` (queue mode) or asking its
    /// workers over `calls` (respond).
    pub(crate) async fn add(
        &self,
        config: &InterfaceConfig,
        intake: Option<Arc<Intake>>,
        calls: Option<tokio::sync::mpsc::Sender<crate::http::HttpCall>>,
    ) -> Result<()> {
        self.look_up(config).await;
        if !matches!(config.source, Source::Http { .. }) {
            return self.start(config, intake).await;
        }
        let route = match (calls, intake) {
            (Some(calls), _) => Route::Respond { interface: config.name.clone(), calls },
            (None, Some(intake)) => Route::Queue(intake),
            (None, None) => anyhow::bail!("interface `{}` has neither an intake nor workers to answer", config.name),
        };
        self.route(config, route).await
    }

    /// Keep the host names in `config`'s `allow:` resolved (those new looked up now).
    async fn look_up(&self, config: &InterfaceConfig) {
        if let Some(allow) = config.source.allow() {
            self.names.add(crate::allow::names_in([allow])).await;
        }
    }

    /// Change interface `old.name`'s source from `old`'s to `new`'s (a deploy in place changed it;
    /// `in_place` let it), feeding `intake`. Nothing to do when they're the same. An `Err` leaves
    /// the old source running (or, for a file pickup or schedule whose new one didn't start,
    /// started again).
    pub(crate) async fn change(
        &self,
        old: &InterfaceConfig,
        new: &InterfaceConfig,
        intake: Option<Arc<Intake>>,
    ) -> Result<()> {
        if old.source == new.source {
            return Ok(());
        }
        let name = &new.name;
        self.look_up(new).await;
        match (&old.source, &new.source) {
            // Its address stays: what it accepts with changes in the running listener.
            (Source::Mllp { host: was_host, port: was_port, .. }, Source::Mllp { host, port, tls, allow })
                if was_host == host && was_port == port =>
            {
                let accepting = {
                    let started = self.started.lock().unwrap_or_else(|e| e.into_inner());
                    started.get(name).and_then(|s| s.listens.as_ref()).map(|l| l.accepting.clone())
                }
                .with_context(|| format!("`{name}`: its MLLP listener isn't running"))?;
                let admission = self.admission(&mllp_label(name), host, *port, allow.as_ref())?;
                let closed = accepting.swap(self.server_tls(tls, &[])?, admission);
                tracing::info!(interface = %name, closed, "its MLLP listener's TLS or allow: changed in place");
                return Ok(());
            }
            (Source::Http { .. }, Source::Http { .. }) => {
                let route = self.route_of(name).with_context(|| format!("`{name}`: its HTTP route isn't running"))?;
                return self.route(new, route).await;
            }
            _ => {}
        }
        // Another kind of source, or an MLLP listener moved. One that listens: its port open
        // before the old source stops. One that doesn't (file, SFTP, schedule): the old one
        // stopped first.
        match &new.source {
            Source::Http { .. } => {
                let intake = intake.with_context(|| format!("interface `{name}` has no intake"))?;
                self.route(new, Route::Queue(intake)).await?;
                if let Some(s) = self.take(name) {
                    self.halt(name, s).await;
                }
            }
            Source::Mllp { .. } => {
                let old_started = self.take(name);
                if let Err(e) = self.start(new, intake).await {
                    if let Some(s) = old_started {
                        self.started.lock().unwrap_or_else(|e| e.into_inner()).insert(name.clone(), s);
                    }
                    return Err(e);
                }
                if let Some(s) = old_started {
                    self.halt(name, s).await;
                }
                let _routing = self.routing.lock().await;
                self.unroute_and_halt(name, None).await;
            }
            _ => {
                self.stop(name).await;
                if let Err(e) = self.start(new, intake.clone()).await {
                    if let Err(again) = self.add(old, intake, None).await {
                        tracing::error!(interface = %name, error = %format!("{again:#}"), "its new source didn't start, nor did its old one again: it receives nothing until the next start");
                    }
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// `config`'s HTTP route, answered by `route`.
    fn endpoint(&self, config: &InterfaceConfig, route: Route) -> Result<Endpoint> {
        let Source::Http { host, port, path, method, tls, allow, .. } = &config.source else {
            anyhow::bail!("interface `{}` has no HTTP source", config.name)
        };
        let protocol = if tls.is_some() { "HTTPS" } else { "HTTP" };
        Ok(Endpoint {
            host: host.clone(),
            port: *port,
            method: method.clone(),
            path: path.clone(),
            route,
            listen: Listen { tls: tls.clone(), allow: allow.clone() },
            tls: self.server_tls(tls, HTTP1)?,
            admission: self.admission(&format!("{protocol} {host}:{port}"), host, *port, allow.as_ref())?,
        })
    }

    /// Interface `name`'s HTTP route now.
    fn route_of(&self, name: &str) -> Option<Route> {
        let servers = self.servers.lock().unwrap_or_else(|e| e.into_inner());
        servers.iter().flat_map(|s| &s.endpoints).find(|e| e.route.interface() == name).map(|e| e.route.clone())
    }

    /// Route `config`'s HTTP source to `route` from now on: on the server for its address (its
    /// route there replaced, or added beside the others), or on a new server listening first;
    /// then taken off any other server (one left with no routes stops).
    async fn route(&self, config: &InterfaceConfig, route: Route) -> Result<()> {
        let mut endpoint = self.endpoint(config, route)?;
        let name = config.name.as_str();
        let address = (endpoint.host.clone(), endpoint.port);
        let _routing = self.routing.lock().await;
        let joined = {
            let mut servers = self.servers.lock().unwrap_or_else(|e| e.into_inner());
            match servers.iter_mut().find(|s| s.host == address.0 && s.port == address.1) {
                Some(server) => {
                    let mut endpoints: Vec<Endpoint> =
                        server.endpoints.iter().filter(|e| e.route.interface() != name).cloned().collect();
                    // The same settings: the server's (its admission counts the refusals).
                    if let Some(same) = server.endpoints.iter().find(|e| e.listen == endpoint.listen) {
                        endpoint.tls = same.tls.clone();
                        endpoint.admission = same.admission.clone();
                    }
                    endpoints.push(endpoint.clone());
                    server.reroute(endpoints, Some(&endpoint))?;
                    true
                }
                None => false,
            }
        };
        if !joined {
            let server = Server::start(address.0.clone(), address.1, vec![endpoint], &self.receiving).await?;
            self.servers.lock().unwrap_or_else(|e| e.into_inner()).push(server);
        }
        self.unroute_and_halt(name, Some(&address)).await;
        Ok(())
    }

    /// Take interface `name`'s route off the server routing it (not the one at `keep`); one left
    /// with no routes stops.
    async fn unroute_and_halt(&self, name: &str, keep: Option<&(String, u16)>) {
        let alone = {
            let mut servers = self.servers.lock().unwrap_or_else(|e| e.into_inner());
            let at = servers.iter().position(|s| {
                keep.is_none_or(|(h, p)| s.host != *h || s.port != *p) && s.interfaces().any(|i| i == name)
            });
            match at {
                None => None,
                Some(at) => {
                    let rest: Vec<Endpoint> =
                        servers[at].endpoints.iter().filter(|e| e.route.interface() != name).cloned().collect();
                    match rest.is_empty() {
                        true => Some(servers.remove(at)),
                        false => {
                            // A subset of routes that worked together: can't collide.
                            if let Err(e) = servers[at].reroute(rest, None) {
                                tracing::error!(interface = %name, error = %format!("{e:#}"), "couldn't take its HTTP route off its server");
                            }
                            None
                        }
                    }
                }
            }
        };
        if let Some(server) = alone {
            stop_listening(name, &server.host, server.port, &server.stop, server.task).await;
        }
    }

    /// Interface `name`'s MLLP listener, file pickup or schedule, no longer kept here.
    fn take(&self, name: &str) -> Option<Started> {
        self.started.lock().unwrap_or_else(|e| e.into_inner()).remove(name)
    }

    /// Stop one taken (`take`): its socket given back, then closed as a drain closes it.
    async fn halt(&self, name: &str, started: Started) {
        match &started.listens {
            Some(l) => stop_listening(name, &l.host, l.port, &started.stop, started.task).await,
            None => {
                started.stop.cancel();
                if tokio::time::timeout(STOP_WITHIN, started.task).await.is_err() {
                    tracing::warn!(interface = %name, "its source didn't stop within {STOP_WITHIN:?}: left to end by itself");
                }
            }
        }
    }

    /// Stop interface `name`'s source (a deploy in place removed it; its gate is closed): its
    /// socket given back, then closed as a drain closes it, waited for up to `STOP_WITHIN`. An
    /// HTTP route is taken off its server, which stops when it routes to nothing else.
    pub(crate) async fn stop(&self, name: &str) {
        if let Some(started) = self.take(name) {
            self.halt(name, started).await;
        }
        let _routing = self.routing.lock().await;
        self.unroute_and_halt(name, None).await;
    }

    /// Every HTTP server's task, to wait for once `receiving` is cancelled (a handover).
    pub(crate) fn take_servers(&self) -> Vec<JoinHandle<()>> {
        std::mem::take(&mut *self.servers.lock().unwrap_or_else(|e| e.into_inner()))
            .into_iter()
            .map(|s| s.task)
            .collect()
    }
}

/// A listener's socket given back (`systemd::release`), then `stop` and its task awaited (up to
/// `STOP_WITHIN`).
async fn stop_listening(name: &str, host: &str, port: u16, stop: &CancellationToken, task: JoinHandle<()>) {
    crate::systemd::release(host, port);
    stop.cancel();
    if tokio::time::timeout(STOP_WITHIN, task).await.is_err() {
        tracing::warn!(interface = %name, "its source didn't stop within {STOP_WITHIN:?}: left to end by itself");
    }
}

/// Run `source` until `stop`, on a task of its own.
fn until(stop: &CancellationToken, source: impl Future<Output = ()> + Send + 'static) -> JoinHandle<()> {
    let stop = stop.clone();
    tokio::spawn(async move {
        stop.run_until_cancelled_owned(source).await;
    })
}

/// Where a source listens, for `in_place`.
#[derive(PartialEq)]
struct Listener<'a> {
    name: &'a str,
    host: &'a str,
    port: u16,
    /// HTTP: its method (upper case) and path.
    route: Option<(String, &'a str)>,
}

impl<'a> Listener<'a> {
    fn of(c: &'a InterfaceConfig) -> Option<Self> {
        let name = c.name.as_str();
        match &c.source {
            Source::Mllp { host, port, .. } => Some(Self { name, host, port: *port, route: None }),
            Source::Http { host, port, path, method, .. } => {
                Some(Self { name, host, port: *port, route: Some((method.to_ascii_uppercase(), path)) })
            }
            _ => None,
        }
    }

    fn what(&self) -> String {
        match &self.route {
            None => format!("MLLP port {}", self.port),
            Some((method, path)) => format!("HTTP {method} {path} on port {}", self.port),
        }
    }
}

/// Whether a deploy can start, change and stop the sources of the interfaces it adds (`added`)
/// and changes (`changed`: each one's running config and its next) in place, with `running`
/// (every interface running now, those it removes too) left as they are; else why only the
/// handover can: a port moving from one interface to another (or an HTTP method and path), a
/// listener changing kind or host on its own port, or an interface changing between queue and
/// respond mode. HTTP routes joining, leaving or changing on a shared server go in place. The set
/// it leaves is checked as a start checks it before this (`check_together`).
pub fn in_place(
    running: &[&InterfaceConfig],
    added: &[&InterfaceConfig],
    changed: &[(&InterfaceConfig, &InterfaceConfig)],
) -> std::result::Result<(), NeedsHandover> {
    for (old, new) in changed {
        if old.responds() != new.responds() {
            return Err(NeedsHandover(format!(
                "`{}` changes between queue and respond mode: its workers are made another way (by the handover)",
                new.name
            )));
        }
    }
    let now: Vec<Listener> = running.iter().filter_map(|c| Listener::of(c)).collect();
    // What listens somewhere new: every added one; a changed one whose address or route moved.
    let moved = changed.iter().filter_map(|(old, new)| {
        let next = Listener::of(new)?;
        (Listener::of(old).as_ref() != Some(&next)).then_some(next)
    });
    for next in added.iter().filter_map(|c| Listener::of(c)).chain(moved) {
        for r in now.iter().filter(|r| r.port == next.port) {
            let http = next.route.is_some() && r.route.is_some() && r.host == next.host;
            if r.name == next.name {
                if http {
                    continue; // its own route moving on its server
                }
                return Err(NeedsHandover(format!(
                    "`{}` listens on port {} another way ({} now, {} next): by the handover, which keeps the socket open",
                    next.name,
                    next.port,
                    r.what(),
                    next.what()
                )));
            }
            if !http {
                return Err(NeedsHandover(format!(
                    "`{}` listens on port {}, which `{}` listens on now: a port moves from one interface to another by \
                     the handover (it keeps the socket open, so no sender is refused)",
                    next.name, next.port, r.name
                )));
            }
            if r.route == next.route {
                return Err(NeedsHandover(format!(
                    "`{}` takes {}, which `{}` answers now: a route moves from one interface to another by the handover",
                    next.name,
                    next.what(),
                    r.name
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(name: &str, source: &str) -> InterfaceConfig {
        let yaml = format!("name: {name}\nsource: {source}\ndestinations:\n  out: {{ file: {{ dir: out }} }}\n");
        ductus_core::yaml::from_value(ductus_core::yaml::parse(&yaml).unwrap()).unwrap()
    }

    fn why(result: std::result::Result<(), NeedsHandover>) -> String {
        result.unwrap_err().0
    }

    /// Sources on ports of their own, files, schedules, routes joining and leaving a shared
    /// server: in place. A port or route moving between interfaces: the handover, saying which.
    #[test]
    fn a_port_moving_between_interfaces_takes_the_handover() {
        let mllp = config("adt", "{ mllp: { port: 6661 } }");
        let api = config("api", "{ http: { port: 8081, path: /a } }");
        let api2 = config("api2", "{ http: { port: 8081, path: /b } }");
        let files = config("files", "{ file: { dir: in } }");
        let running = [&mllp, &api, &api2];

        let new_mllp = config("orders", "{ mllp: { port: 6662 } }");
        let own_http = config("fhir", "{ http: { port: 8082, path: /fhir, mode: respond } }");
        assert!(in_place(&running, &[&new_mllp, &own_http, &files], &[]).is_ok());
        let joins = config("api3", "{ http: { port: 8081, path: /c } }");
        let a = config("a", "{ http: { port: 9001, path: /a } }");
        let b = config("b", "{ http: { port: 9001, path: /b } }");
        assert!(in_place(&running, &[&joins, &a, &b], &[]).is_ok(), "joining a server, or two on a new one");

        let moved = config("adt2", "{ mllp: { port: 6661 } }");
        let why_ = why(in_place(&running, &[&moved], &[]));
        assert!(why_.starts_with("`adt2` listens on port 6661, which `adt` listens on now"), "{why_}");
        let on_mllp = config("web", "{ http: { port: 6661, path: /a } }");
        assert!(why(in_place(&running, &[&on_mllp], &[])).contains("which `adt` listens on now"));
        let takes = config("api3", "{ http: { port: 8081, path: /a } }");
        let why_ = why(in_place(&running, &[&takes], &[]));
        assert!(why_.starts_with("`api3` takes HTTP POST /a on port 8081, which `api` answers now"), "{why_}");
    }

    /// Changed sources: a moved MLLP port, TLS or `allow:` on the same port, a route moved on its
    /// server or to another, a file pickup becoming a listener: in place. A port swapped between
    /// two interfaces, a listener changing kind on its own port, queue ↔ respond: the handover.
    #[test]
    fn a_changed_source_goes_in_place_unless_a_port_moves_between_interfaces() {
        let mllp = config("adt", "{ mllp: { port: 6661 } }");
        let lab = config("lab", "{ mllp: { port: 6662 } }");
        let api = config("api", "{ http: { port: 8081, path: /a } }");
        let api2 = config("api2", "{ http: { port: 8081, path: /b } }");
        let files = config("files", "{ file: { dir: in } }");
        let running = [&mllp, &lab, &api, &api2, &files];
        let ok = |old: &InterfaceConfig, new: &InterfaceConfig| in_place(&running, &[], &[(old, new)]);

        assert!(ok(&mllp, &config("adt", "{ mllp: { port: 6671 } }")).is_ok(), "moved");
        let allow = config("adt", "{ mllp: { port: 6661, allow: [10.0.0.0/8] } }");
        assert!(ok(&mllp, &allow).is_ok(), "allow: on the same port");
        assert!(ok(&api, &config("api", "{ http: { port: 8081, path: /c } }")).is_ok(), "its route on its server");
        assert!(ok(&api, &config("api", "{ http: { port: 8090, path: /a } }")).is_ok(), "to a server of its own");
        assert!(ok(&files, &config("files", "{ mllp: { port: 6680 } }")).is_ok(), "a pickup becomes a listener");
        assert!(ok(&files, &config("files", "{ file: { dir: other } }")).is_ok());

        let (adt_next, lab_next) =
            (config("adt", "{ mllp: { port: 6662 } }"), config("lab", "{ mllp: { port: 6661 } }"));
        let swapped = in_place(&running, &[], &[(&mllp, &adt_next), (&lab, &lab_next)]);
        assert!(why(swapped).contains("which `lab` listens on now"));
        let kind = why(ok(&mllp, &config("adt", "{ http: { port: 6661, path: /adt } }")));
        assert!(kind.starts_with("`adt` listens on port 6661 another way (MLLP port 6661 now"), "{kind}");
        let takes = why(ok(&api, &config("api", "{ http: { port: 8081, path: /b } }")));
        assert!(takes.contains("which `api2` answers now"), "{takes}");
        let respond = why(ok(&api, &config("api", "{ http: { port: 8081, path: /a, mode: respond } }")));
        assert!(respond.starts_with("`api` changes between queue and respond mode"), "{respond}");
    }
}
