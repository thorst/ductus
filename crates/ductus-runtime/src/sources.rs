//! Each interface's source (what it receives with: an MLLP listener, file or SFTP pickup, a
//! schedule, an HTTP route), started at the engine's start, or alone when a deploy in place adds
//! the interface, and stopped alone when one removes it: the other interfaces' sources never
//! notice (0.7.5.2.1).
//!
//! Each MLLP listener, file pickup and schedule has a stop of its own (a child of the engine's
//! run). HTTP interfaces sharing a port share one server (one router), so one is added or removed
//! in place only **on a port of its own**: its own server, stopped alone. One that would share a
//! port with another, and an interface listening on a port another one listens on now (a port
//! moving from a removed interface to an added one), go by the handover (`in_place`): this
//! engine can't hand a socket from one listener to the next without refusing a connection, the
//! handover can.
//!
//! Stopping one (`stop`): its listening socket is given back first (`systemd::release`: neither
//! systemd nor this process keeps it, so the port is free), then the listener closes the way a
//! drain closes it: MLLP connections finish the message in hand (stored and ACKed) and close; an
//! HTTP server answers the requests in flight; file pickup and schedules stop between messages.

use crate::http::{Endpoint, Server};
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
    /// Each HTTP server, with the interfaces it routes to.
    servers: Mutex<Vec<Server>>,
}

/// One interface's MLLP listener, file pickup or schedule.
struct Started {
    stop: CancellationToken,
    task: JoinHandle<()>,
    /// MLLP: where it listens.
    listens: Option<(String, u16)>,
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
    /// is served with the others on its port: `serve`; an internal one has none). The first bind
    /// is part of it: a port in use is an error.
    pub(crate) async fn start(&self, config: &InterfaceConfig, intake: Option<Arc<Intake>>) -> Result<()> {
        let name = &config.name;
        let intake = || intake.clone().with_context(|| format!("interface `{name}` has no intake"));
        let stop = self.ended.child_token();
        let (task, listens) = match &config.source {
            Source::Mllp { host, port, tls, allow } => {
                let label = format!("interface `{name}` (MLLP)");
                let admission = self.admission(&label, host, *port, allow.as_ref())?;
                let tls = self.server_tls(tls, &[])?;
                (crate::ingest::spawn_mllp(intake()?, tls, admission, stop.clone()).await?, Some((host.clone(), *port)))
            }
            Source::File(f) | Source::Sftp(f) => {
                let server = f.server.as_deref().and_then(|s| self.sftp.get(s).cloned());
                let watcher = crate::files::FileWatcher::new(intake()?, server, self.ownership.clone())?;
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
    /// host names in its `allow:` looked up first, then as `start`; an HTTP one on a server of
    /// its own, storing into `intake` (queue mode) or asking its workers over `calls` (respond).
    pub(crate) async fn add(
        &self,
        config: &InterfaceConfig,
        intake: Option<Arc<Intake>>,
        calls: Option<tokio::sync::mpsc::Sender<crate::http::HttpCall>>,
    ) -> Result<()> {
        if let Some(allow) = config.source.allow() {
            self.names.add(crate::allow::names_in([allow])).await;
        }
        let Source::Http { host, port, path, method, tls, allow, .. } = &config.source else {
            return self.start(config, intake).await;
        };
        let route = match (calls, intake) {
            (Some(calls), _) => crate::http::Route::Respond { interface: config.name.clone(), calls },
            (None, Some(intake)) => crate::http::Route::Queue(intake),
            (None, None) => anyhow::bail!("interface `{}` has neither an intake nor workers to answer", config.name),
        };
        let protocol = if tls.is_some() { "HTTPS" } else { "HTTP" };
        let endpoint = Endpoint {
            host: host.clone(),
            port: *port,
            method: method.clone(),
            path: path.clone(),
            route,
            tls: self.server_tls(tls, HTTP1)?,
            admission: self.admission(&format!("{protocol} {host}:{port}"), host, *port, allow.as_ref())?,
        };
        self.serve(vec![endpoint]).await
    }

    /// Stop interface `name`'s source (a deploy in place removed it; its gate is closed): its
    /// socket given back, then closed as a drain closes it, waited for up to `STOP_WITHIN`. An
    /// HTTP server stops only when it routes to nothing else (`in_place` made sure); one shared
    /// is left (said).
    pub(crate) async fn stop(&self, name: &str) {
        let started = self.started.lock().unwrap_or_else(|e| e.into_inner()).remove(name);
        let server = {
            let mut servers = self.servers.lock().unwrap_or_else(|e| e.into_inner());
            match servers.iter().position(|s| s.interfaces.iter().any(|i| i == name)) {
                Some(at) if servers[at].interfaces.len() == 1 => Some(servers.remove(at)),
                Some(at) => {
                    let s = &servers[at];
                    tracing::warn!(interface = %name, port = s.port, "its HTTP port is shared: its route stays until the next handover");
                    None
                }
                None => None,
            }
        };
        let (stop, task, listens) = match (started, server) {
            (Some(s), _) => (s.stop, s.task, s.listens),
            (None, Some(s)) => (s.stop, s.task, Some((s.host, s.port))),
            (None, None) => return,
        };
        if let Some((host, port)) = &listens {
            crate::systemd::release(host, *port);
        }
        stop.cancel();
        if tokio::time::timeout(STOP_WITHIN, task).await.is_err() {
            tracing::warn!(interface = %name, "its source didn't stop within {STOP_WITHIN:?}: left to end by itself");
        }
    }

    /// Every HTTP server's task, to wait for once `receiving` is cancelled (a handover).
    pub(crate) fn take_servers(&self) -> Vec<JoinHandle<()>> {
        std::mem::take(&mut *self.servers.lock().unwrap_or_else(|e| e.into_inner()))
            .into_iter()
            .map(|s| s.task)
            .collect()
    }
}

/// Run `source` until `stop`, on a task of its own.
fn until(stop: &CancellationToken, source: impl Future<Output = ()> + Send + 'static) -> JoinHandle<()> {
    let stop = stop.clone();
    tokio::spawn(async move {
        stop.run_until_cancelled_owned(source).await;
    })
}

/// Whether a deploy can start and stop the sources of the interfaces it adds (`added`) and
/// removes (`removed`, by name) in place, with `running` (every interface running now, those
/// removed too) left as they are; else why only the handover can (a port moving from one
/// interface to another, or HTTP interfaces sharing a port). The set it leaves is checked as a
/// start checks it before this (`check_together`).
pub fn in_place(
    running: &[&InterfaceConfig],
    added: &[&InterfaceConfig],
    removed: &[&str],
) -> std::result::Result<(), NeedsHandover> {
    let port = |c: &InterfaceConfig| match &c.source {
        Source::Mllp { port, .. } | Source::Http { port, .. } => Some(*port),
        _ => None,
    };
    let http = |c: &InterfaceConfig| matches!(c.source, Source::Http { .. });
    for (i, a) in added.iter().enumerate() {
        let Some(p) = port(a) else { continue };
        if let Some(r) = running.iter().find(|r| port(r) == Some(p)) {
            return Err(NeedsHandover(format!(
                "`{}` listens on port {p}, which `{}` listens on now: a port moves from one interface to another by \
                 the handover (it keeps the socket open, so no sender is refused)",
                a.name, r.name
            )));
        }
        if let Some(b) = added[i + 1..].iter().find(|b| port(b) == Some(p)) {
            return Err(NeedsHandover(format!(
                "`{}` and `{}` share HTTP port {p}: interfaces sharing a port are added by the handover",
                a.name, b.name
            )));
        }
    }
    for name in removed {
        let Some(r) = running.iter().find(|r| r.name == *name) else { continue };
        if !http(r) {
            continue;
        }
        let p = port(r);
        if let Some(other) = running.iter().find(|o| o.name != r.name && port(o) == p) {
            return Err(NeedsHandover(format!(
                "`{}` shares HTTP port {} with `{}`: an interface on a shared port is removed by the handover",
                r.name,
                p.unwrap_or_default(),
                other.name
            )));
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

    /// Sources on ports of their own, files, schedules: in place. A port moving between
    /// interfaces, or shared HTTP ports added or removed: the handover, saying which.
    #[test]
    fn a_port_moving_or_shared_takes_the_handover() {
        let mllp = config("adt", "{ mllp: { port: 6661 } }");
        let api = config("api", "{ http: { port: 8081, path: /a } }");
        let api2 = config("api2", "{ http: { port: 8081, path: /b } }");
        let files = config("files", "{ file: { dir: in } }");
        let running = [&mllp, &api, &api2];

        let new_mllp = config("orders", "{ mllp: { port: 6662 } }");
        let own_http = config("fhir", "{ http: { port: 8082, path: /fhir, mode: respond } }");
        assert!(in_place(&running, &[&new_mllp, &own_http, &files], &["adt"]).is_ok());

        let moved = config("adt2", "{ mllp: { port: 6661 } }");
        let why = in_place(&running, &[&moved], &["adt"]).unwrap_err().0;
        assert!(why.starts_with("`adt2` listens on port 6661, which `adt` listens on now"), "{why}");
        let joins = config("api3", "{ http: { port: 8081, path: /c } }");
        assert!(in_place(&running, &[&joins], &[]).unwrap_err().0.contains("which `api` listens on now"));

        let a = config("a", "{ http: { port: 9001, path: /a } }");
        let b = config("b", "{ http: { port: 9001, path: /b } }");
        assert!(in_place(&running, &[&a, &b], &[]).unwrap_err().0.starts_with("`a` and `b` share HTTP port 9001"));

        let why = in_place(&running, &[], &["api"]).unwrap_err().0;
        assert!(why.starts_with("`api` shares HTTP port 8081 with `api2`"), "{why}");
        assert!(in_place(&[&mllp, &own_http], &[], &["fhir", "adt"]).is_ok(), "alone on its port");
    }
}
