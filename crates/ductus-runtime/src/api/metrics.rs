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

    out.family("ductus_start_time_seconds", "gauge", "When the engine started (Unix time).").sample(
        "ductus_start_time_seconds",
        "",
        engine.started_at.timestamp(),
    );
    let draining = !matches!(engine.drain_state(), DrainState::Accepting);
    out.family("ductus_draining", "gauge", "1 while draining or drained (not taking new messages).").sample(
        "ductus_draining",
        "",
        u8::from(draining),
    );

    out.family("ductus_interface_stopped", "gauge", "1 if the interface was stopped because its code ran away.");
    for i in &engine.interfaces {
        out.sample("ductus_interface_stopped", &iface(&i.config.name), u8::from(i.health.stopped().is_some()));
    }
    let queued = || engine.interfaces.iter().filter(|i| !i.config.responds());
    for (name, help, value) in [
        (
            "ductus_messages_received_total",
            "Messages stored and queued.",
            (|s: &stats::InterfaceStats| stats::get(&s.received)) as fn(&stats::InterfaceStats) -> u64,
        ),
        ("ductus_messages_duplicate_total", "Messages stored as duplicates (not queued).", |s| {
            stats::get(&s.duplicates)
        }),
        (
            "ductus_messages_store_failures_total",
            "Messages that couldn't be stored (the sender was told to retry).",
            |s| stats::get(&s.store_failures),
        ),
    ] {
        out.family(name, "counter", help);
        for i in queued() {
            out.sample(name, &iface(&i.config.name), value(&i.stats));
        }
    }

    out.family("ductus_deliveries_total", "counter", "Messages a destination finished, by outcome.");
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
    out.family("ductus_delivery_retries_total", "counter", "Temporary failures (each retry counts).");
    for i in queued() {
        for d in &i.stats.destinations {
            out.sample("ductus_delivery_retries_total", &dest(&i.config.name, &d.name), stats::get(&d.retries));
        }
    }
    for (name, help, pick) in [
        (
            "ductus_delivery_latency_seconds",
            "From receipt to sent, waiting in the queue included.",
            (|d: &stats::DestinationStats| &d.latency) as fn(&stats::DestinationStats) -> &stats::Histogram,
        ),
        ("ductus_send_seconds", "Each successful send: the receiver's time.", |d| &d.send),
        ("ductus_transform_seconds", "The destination's function, per message.", |d| &d.transform),
    ] {
        out.family(name, "histogram", help);
        for i in queued() {
            for d in &i.stats.destinations {
                out.histogram(name, &dest(&i.config.name, &d.name), pick(d));
            }
        }
    }

    out.family("ductus_queue_pending", "gauge", "Messages waiting for a destination.");
    for i in queued() {
        for ((name, id), _) in i.destinations.iter().zip(&i.stats.destinations) {
            out.sample("ductus_queue_pending", &dest(&i.config.name, name), snapshot.queue(*id).pending);
        }
    }
    out.family("ductus_queue_quarantined", "gauge", "Messages quarantined by a destination (needing a person).");
    for i in queued() {
        for (name, id) in &i.destinations {
            out.sample("ductus_queue_quarantined", &dest(&i.config.name, name), snapshot.queue(*id).quarantined);
        }
    }
    out.family(
        "ductus_queue_oldest_seconds",
        "gauge",
        "How long the oldest message waiting for a destination has waited.",
    );
    for i in queued() {
        for (name, id) in &i.destinations {
            let age = snapshot.oldest_age(*id).map_or(0.0, |a| a.as_secs_f64());
            out.sample("ductus_queue_oldest_seconds", &dest(&i.config.name, name), age);
        }
    }
    let now = chrono::Utc::now();
    out.family(
        "ductus_destination_state",
        "gauge",
        "0 ok, 1 behind (past its `behind` limit), 2 error (quarantined messages), 3 stalled, 4 paused.",
    );
    for i in queued() {
        for (n, (name, _)) in i.destinations.iter().enumerate() {
            let state = engine.diagnose(i, n, &snapshot, now).state as u8;
            out.sample("ductus_destination_state", &dest(&i.config.name, name), state);
        }
    }
    out.family(
        "ductus_interface_state",
        "gauge",
        "0 ok, 1 silent (nothing within `active_within`), 2 missed (nothing by `active_by`), 3 stopped.",
    );
    for i in queued() {
        out.sample("ductus_interface_state", &iface(&i.config.name), engine.interface_state(i, now).0 as u8);
    }
    out.family("ductus_destination_paused", "gauge", "1 while a destination is paused on purpose (`ductus pause`).");
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
        out.family(
            "ductus_held_messages",
            "gauge",
            "Unfinished messages past their retention, in the held area (as of the hourly retention run).",
        );
        for i in &last.held.interfaces {
            let labels = iface(&i.interface);
            out.sample("ductus_held_messages", &format!("{labels},status=\"pending\""), i.pending).sample(
                "ductus_held_messages",
                &format!("{labels},status=\"quarantined\""),
                i.quarantined,
            );
        }
        out.family("ductus_held_bytes", "gauge", "Disk used by the held area (as of the hourly retention run).")
            .sample("ductus_held_bytes", "", last.held.months.iter().map(|m| m.bytes).sum::<i64>());
    }

    let responding = || engine.interfaces.iter().filter(|i| i.config.responds());
    out.family("ductus_http_request_seconds", "histogram", "Respond-mode HTTP requests: time in the engine.");
    for i in responding() {
        out.histogram("ductus_http_request_seconds", &iface(&i.config.name), &i.stats.requests);
    }
    out.family("ductus_http_request_errors_total", "counter", "Respond-mode HTTP requests that failed.");
    for i in responding() {
        out.sample("ductus_http_request_errors_total", &iface(&i.config.name), stats::get(&i.stats.request_errors));
    }

    out.family("ductus_resource_call_seconds", "histogram", "Calls from interface code to a database or API.");
    for (kind, name, s) in engine.calls.all() {
        out.histogram(
            "ductus_resource_call_seconds",
            &format!("kind=\"{kind}\",resource=\"{}\"", label(name)),
            &s.calls,
        );
    }
    out.family("ductus_resource_call_errors_total", "counter", "Calls that failed (network, SQL, timeout).");
    for (kind, name, s) in engine.calls.all() {
        let labels = format!("kind=\"{kind}\",resource=\"{}\"", label(name));
        out.sample("ductus_resource_call_errors_total", &labels, stats::get(&s.errors));
    }

    Ok(([(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")], out.finish()))
}
