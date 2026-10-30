//! Prometheus metrics: `GET /api/v1/metrics` in Prometheus's text format, for Prometheus,
//! Grafana Agent, Datadog, and anything else that scrapes it. Needs a viewer token like every
//! other call (Prometheus: `authorization: { credentials: dux_... }` and
//! `metrics_path: /api/v1/metrics` in the scrape config).
//!
//! Counters and histograms count since the engine started (`rate()` handles restarts); queue
//! sizes come from the database at each scrape (one query, from the queue indexes).

use super::{ApiError, Caller, Eng, Shared};
use crate::control::DrainState;
use crate::stats::{self, Exposition, label};
use axum::http::header;
use axum::response::IntoResponse;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn routes() -> OpenApiRouter<Shared> {
    OpenApiRouter::new().routes(routes!(metrics))
}

/// A family of series `/metrics` serves.
pub struct Metric {
    pub name: &'static str,
    /// `counter`, `gauge` or `histogram`.
    pub kind: &'static str,
    /// The labels each series has.
    pub labels: &'static str,
    /// What it measures (the HELP line, and the reference page).
    pub help: &'static str,
}

/// Every family `/metrics` serves, in the order it serves them: the reference page (`ductus
/// reference`) is made from this, and serving a family that isn't here panics in the tests.
pub const METRICS: &[Metric] = &[
    Metric {
        name: "ductus_start_time_seconds",
        kind: "gauge",
        labels: "",
        help: "When the engine started (Unix time).",
    },
    Metric {
        name: "ductus_draining",
        kind: "gauge",
        labels: "",
        help: "1 while draining or drained (not taking new messages).",
    },
    Metric {
        name: "ductus_alerts_open",
        kind: "gauge",
        labels: "severity",
        help: "Open alerts, by severity. Only with `alerts:` in ductus.yaml.",
    },
    Metric {
        name: "ductus_alert_send_failures_total",
        kind: "counter",
        labels: "channel",
        help: "Alert notifications that couldn't be sent (each try counts), by channel (`owner`: owners' addresses). A failing channel can't alert about itself: watch this.",
    },
    Metric {
        name: "ductus_certificate_expiry_seconds",
        kind: "gauge",
        labels: "side, resource, host, port",
        help: "Seconds until the earliest expiry in a certificate chain (negative once expired): a server's as of its last check (side remote), or one the engine presents (side ours: host is this server, port 0).",
    },
    Metric {
        name: "ductus_interface_stopped",
        kind: "gauge",
        labels: "interface",
        help: "1 if the interface was stopped because its code ran away.",
    },
    Metric {
        name: "ductus_messages_received_total",
        kind: "counter",
        labels: "interface",
        help: "Messages stored and queued.",
    },
    Metric {
        name: "ductus_messages_duplicate_total",
        kind: "counter",
        labels: "interface",
        help: "Messages stored as duplicates (not queued).",
    },
    Metric {
        name: "ductus_messages_store_failures_total",
        kind: "counter",
        labels: "interface",
        help: "Messages that couldn't be stored (the sender was told to retry).",
    },
    Metric {
        name: "ductus_deliveries_total",
        kind: "counter",
        labels: "interface, destination, outcome",
        help: "Messages a destination finished, by outcome (sent, filtered, quarantined).",
    },
    Metric {
        name: "ductus_delivery_retries_total",
        kind: "counter",
        labels: "interface, destination",
        help: "Temporary failures (each retry counts).",
    },
    Metric {
        name: "ductus_delivery_latency_seconds",
        kind: "histogram",
        labels: "interface, destination",
        help: "From receipt to sent, waiting in the queue included.",
    },
    Metric {
        name: "ductus_send_seconds",
        kind: "histogram",
        labels: "interface, destination",
        help: "Each successful send: the receiver's time.",
    },
    Metric {
        name: "ductus_transform_seconds",
        kind: "histogram",
        labels: "interface, destination",
        help: "The destination's function, per message.",
    },
    Metric {
        name: "ductus_queue_pending",
        kind: "gauge",
        labels: "interface, destination",
        help: "Messages waiting for a destination.",
    },
    Metric {
        name: "ductus_queue_quarantined",
        kind: "gauge",
        labels: "interface, destination",
        help: "Messages quarantined by a destination (needing a person).",
    },
    Metric {
        name: "ductus_queue_oldest_seconds",
        kind: "gauge",
        labels: "interface, destination",
        help: "How long the oldest message waiting for a destination has waited.",
    },
    Metric {
        name: "ductus_destination_state",
        kind: "gauge",
        labels: "interface, destination",
        help: "0 ok, 1 behind (past its `behind` limit), 2 error (quarantined messages), 3 stalled, 4 paused.",
    },
    Metric {
        name: "ductus_interface_state",
        kind: "gauge",
        labels: "interface",
        help: "0 ok, 1 silent (nothing within `active_within`), 2 missed (nothing by `active_by`), 3 stopped.",
    },
    Metric {
        name: "ductus_destination_paused",
        kind: "gauge",
        labels: "interface, destination",
        help: "1 while a destination is paused on purpose (`ductus pause`).",
    },
    Metric {
        name: "ductus_held_messages",
        kind: "gauge",
        labels: "interface, status",
        help: "Unfinished messages past their retention, in the held area (as of the hourly retention run).",
    },
    Metric {
        name: "ductus_held_bytes",
        kind: "gauge",
        labels: "",
        help: "Disk used by the held area (as of the hourly retention run).",
    },
    Metric {
        name: "ductus_http_request_seconds",
        kind: "histogram",
        labels: "interface",
        help: "Respond-mode HTTP requests: time in the engine.",
    },
    Metric {
        name: "ductus_http_request_errors_total",
        kind: "counter",
        labels: "interface",
        help: "Respond-mode HTTP requests that failed.",
    },
    Metric {
        name: "ductus_resource_call_seconds",
        kind: "histogram",
        labels: "kind, resource",
        help: "Calls from interface code to a database or API.",
    },
    Metric {
        name: "ductus_resource_call_errors_total",
        kind: "counter",
        labels: "kind, resource",
        help: "Calls that failed (network, SQL, timeout).",
    },
];

/// Start a family from `METRICS`.
fn family<'a>(out: &'a mut Exposition, name: &str) -> &'a mut Exposition {
    let m = METRICS.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("{name} isn't in METRICS"));
    out.family(m.name, m.kind, m.help)
}

/// Metrics in Prometheus's text format: messages received and delivered, retries, timings
/// (receipt to sent, sends, transforms, respond-mode requests, database and API calls), queue
/// sizes, stopped interfaces, drain state.
#[utoipa::path(
    get, path = "/metrics", tag = "engine",
    responses((status = 200, description = "Prometheus text format (0.0.4)", body = String, content_type = "text/plain"))
)]
async fn metrics(engine: Eng, _caller: Caller) -> Result<impl IntoResponse, ApiError> {
    let snapshot = engine.snapshot().await?;
    let mut out = Exposition::new();
    let iface = |name: &str| format!("interface=\"{}\"", label(name));
    let dest = |i: &str, d: &str| format!("interface=\"{}\",destination=\"{}\"", label(i), label(d));

    family(&mut out, "ductus_start_time_seconds").sample(
        "ductus_start_time_seconds",
        "",
        engine.started_at.timestamp(),
    );
    let draining = !matches!(engine.drain_state(), DrainState::Accepting);
    family(&mut out, "ductus_draining").sample("ductus_draining", "", u8::from(draining));

    if let Some(a) = &engine.alerts {
        let rows: Vec<_> =
            engine.store.alerts().await?.into_iter().filter(|r| super::alerts::ours(&engine, r)).collect();
        family(&mut out, "ductus_alerts_open");
        for severity in ["warning", "critical"] {
            let open = rows.iter().filter(|r| r.is_open() && r.severity.as_deref() == Some(severity)).count();
            out.sample("ductus_alerts_open", &format!("severity=\"{severity}\""), open);
        }
        family(&mut out, "ductus_alert_send_failures_total");
        for (channel, failed) in a.failures() {
            out.sample("ductus_alert_send_failures_total", &format!("channel=\"{}\"", label(&channel)), failed);
        }
    }
    if !engine.certificates.is_empty() {
        use crate::certificates::Side;
        // Ours are read from memory; the table only for the servers the engine checks.
        let seen = if engine.certificates.iter().any(|t| t.side == Side::Remote) {
            engine.store.certificates().await?
        } else {
            Vec::new()
        };
        family(&mut out, "ductus_certificate_expiry_seconds");
        let now = chrono::Utc::now();
        for t in &engine.certificates {
            let ends = match crate::certificates::our_row(t, &engine.tls, now) {
                Some(ours) => ours.chain_min_not_after,
                None => seen
                    .iter()
                    .find(|c| c.resource == t.resource && c.host == t.host && c.port == i32::from(t.port))
                    .and_then(|c| c.chain_min_not_after),
            };
            if let Some(ends) = ends {
                let labels = format!(
                    "side=\"{}\",resource=\"{}\",host=\"{}\",port=\"{}\"",
                    t.side.label(),
                    label(&t.resource),
                    label(&t.host),
                    t.port
                );
                out.sample("ductus_certificate_expiry_seconds", &labels, (ends - now).num_seconds());
            }
        }
    }
    family(&mut out, "ductus_interface_stopped");
    for i in &engine.interfaces {
        out.sample("ductus_interface_stopped", &iface(&i.config.name), u8::from(i.health.stopped().is_some()));
    }
    let queued = || engine.interfaces.iter().filter(|i| !i.config.responds());
    for (name, value) in [
        (
            "ductus_messages_received_total",
            (|s: &stats::InterfaceStats| stats::get(&s.received)) as fn(&stats::InterfaceStats) -> u64,
        ),
        ("ductus_messages_duplicate_total", |s| stats::get(&s.duplicates)),
        ("ductus_messages_store_failures_total", |s| stats::get(&s.store_failures)),
    ] {
        family(&mut out, name);
        for i in queued() {
            out.sample(name, &iface(&i.config.name), value(&i.stats));
        }
    }

    family(&mut out, "ductus_deliveries_total");
    for i in queued() {
        for d in &i.stats.destinations {
            let labels = dest(&i.config.name, &d.name);
            out.sample("ductus_deliveries_total", &format!("{labels},outcome=\"sent\""), stats::get(&d.sent))
                .sample("ductus_deliveries_total", &format!("{labels},outcome=\"filtered\""), stats::get(&d.filtered))
                .sample(
                    "ductus_deliveries_total",
                    &format!("{labels},outcome=\"quarantined\""),
                    stats::get(&d.quarantined),
                );
        }
    }
    family(&mut out, "ductus_delivery_retries_total");
    for i in queued() {
        for d in &i.stats.destinations {
            out.sample("ductus_delivery_retries_total", &dest(&i.config.name, &d.name), stats::get(&d.retries));
        }
    }
    for (name, pick) in [
        (
            "ductus_delivery_latency_seconds",
            (|d: &stats::DestinationStats| &d.latency) as fn(&stats::DestinationStats) -> &stats::Histogram,
        ),
        ("ductus_send_seconds", |d| &d.send),
        ("ductus_transform_seconds", |d| &d.transform),
    ] {
        family(&mut out, name);
        for i in queued() {
            for d in &i.stats.destinations {
                out.histogram(name, &dest(&i.config.name, &d.name), pick(d));
            }
        }
    }

    family(&mut out, "ductus_queue_pending");
    for i in queued() {
        for ((name, id), _) in i.destinations.iter().zip(&i.stats.destinations) {
            out.sample("ductus_queue_pending", &dest(&i.config.name, name), snapshot.queue(*id).pending);
        }
    }
    family(&mut out, "ductus_queue_quarantined");
    for i in queued() {
        for (name, id) in &i.destinations {
            out.sample("ductus_queue_quarantined", &dest(&i.config.name, name), snapshot.queue(*id).quarantined);
        }
    }
    family(&mut out, "ductus_queue_oldest_seconds");
    for i in queued() {
        for (name, id) in &i.destinations {
            let age = snapshot.oldest_age(*id).map_or(0.0, |a| a.as_secs_f64());
            out.sample("ductus_queue_oldest_seconds", &dest(&i.config.name, name), age);
        }
    }
    let now = chrono::Utc::now();
    family(&mut out, "ductus_destination_state");
    for i in queued() {
        for (n, (name, _)) in i.destinations.iter().enumerate() {
            let state = engine.diagnose(i, n, &snapshot, now).state as u8;
            out.sample("ductus_destination_state", &dest(&i.config.name, name), state);
        }
    }
    family(&mut out, "ductus_interface_state");
    for i in queued() {
        out.sample("ductus_interface_state", &iface(&i.config.name), engine.interface_state(i, now).0 as u8);
    }
    family(&mut out, "ductus_destination_paused");
    for i in queued() {
        for (name, id) in &i.destinations {
            out.sample(
                "ductus_destination_paused",
                &dest(&i.config.name, name),
                u8::from(engine.paused(*id).is_some()),
            );
        }
    }

    // Counted by the hourly retention run (counting every held row at each scrape could be slow).
    if let Some(last) = engine.retention.last_drop() {
        family(&mut out, "ductus_held_messages");
        for i in &last.held.interfaces {
            let labels = iface(&i.interface);
            out.sample("ductus_held_messages", &format!("{labels},status=\"pending\""), i.pending).sample(
                "ductus_held_messages",
                &format!("{labels},status=\"quarantined\""),
                i.quarantined,
            );
        }
        family(&mut out, "ductus_held_bytes").sample(
            "ductus_held_bytes",
            "",
            last.held.months.iter().map(|m| m.bytes).sum::<i64>(),
        );
    }

    let responding = || engine.interfaces.iter().filter(|i| i.config.responds());
    family(&mut out, "ductus_http_request_seconds");
    for i in responding() {
        out.histogram("ductus_http_request_seconds", &iface(&i.config.name), &i.stats.requests);
    }
    family(&mut out, "ductus_http_request_errors_total");
    for i in responding() {
        out.sample("ductus_http_request_errors_total", &iface(&i.config.name), stats::get(&i.stats.request_errors));
    }

    family(&mut out, "ductus_resource_call_seconds");
    for (kind, name, s) in engine.calls.all() {
        out.histogram(
            "ductus_resource_call_seconds",
            &format!("kind=\"{kind}\",resource=\"{}\"", label(name)),
            &s.calls,
        );
    }
    family(&mut out, "ductus_resource_call_errors_total");
    for (kind, name, s) in engine.calls.all() {
        let labels = format!("kind=\"{kind}\",resource=\"{}\"", label(name));
        out.sample("ductus_resource_call_errors_total", &labels, stats::get(&s.errors));
    }

    Ok(([(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")], out.finish()))
}
