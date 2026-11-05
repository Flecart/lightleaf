//! Prometheus metrics at `GET /metrics`.
//!
//! Access is allowed from the server itself, or from anywhere with
//! `Authorization: Bearer $LIGHTLEAF_METRICS_TOKEN` when that variable is set.

use crate::{security, state::SharedState};
use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::{
    fmt::Write,
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

/// Counters updated on hot paths (one relaxed atomic add each).
#[derive(Default)]
pub struct Metrics {
    pub compiles_success: AtomicU64,
    pub compiles_errors: AtomicU64,
    pub compiles_failure: AtomicU64,
    pub compiles_timeout: AtomicU64,
    pub compiles_coalesced: AtomicU64,
    pub compiles_rejected: AtomicU64,
    pub compile_millis: AtomicU64,
    pub logins_ok: AtomicU64,
    pub logins_failed: AtomicU64,
    pub auth_rate_limited: AtomicU64,
    pub ws_connections: AtomicU64,
    pub ws_slow_disconnects: AtomicU64,
    pub ws_updates: AtomicU64,
}

impl Metrics {
    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Relaxed);
    }

    pub fn record_compile(&self, status: &str, millis: u64) {
        let c = match status {
            "success" => &self.compiles_success,
            "errors" => &self.compiles_errors,
            "timeout" => &self.compiles_timeout,
            _ => &self.compiles_failure,
        };
        Self::inc(c);
        self.compile_millis.fetch_add(millis, Relaxed);
    }
}

fn authorized(state: &SharedState, ci: &ConnectInfo<SocketAddr>, headers: &HeaderMap) -> bool {
    if let Some(token) = &state.cfg.metrics_token {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        if given.is_some_and(|g| constant_time_eq(g.as_bytes(), token.as_bytes())) {
            return true;
        }
    }
    security::is_local_request(Some(security::peer(ci)), headers)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Resident memory of this process in bytes (Linux only).
fn rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

pub async fn handler(
    State(state): State<SharedState>,
    ci: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !authorized(&state, &ci, &headers) {
        return (StatusCode::FORBIDDEN, "forbidden\n").into_response();
    }
    let (users, projects, sessions) = state
        .db(|c| {
            let n = |q: &str| c.query_row(q, [], |r| r.get::<_, i64>(0));
            Ok((
                n("SELECT COUNT(*) FROM users")?,
                n("SELECT COUNT(*) FROM projects")?,
                n("SELECT COUNT(*) FROM sessions")?,
            ))
        })
        .await
        .unwrap_or((0, 0, 0));
    let hub = state.hub.stats();
    let m = &state.metrics;
    let running = state.cfg.max_concurrent_compiles - state.compile_slots.available_permits();

    let mut out = String::new();
    let mut gauge = |name: &str, help: &str, v: u64| {
        let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {v}");
    };
    gauge("lightleaf_users", "Registered accounts.", users as u64);
    gauge("lightleaf_projects", "Projects.", projects as u64);
    gauge(
        "lightleaf_sessions",
        "Stored login sessions.",
        sessions as u64,
    );
    gauge(
        "lightleaf_rooms",
        "Projects open in at least one browser.",
        hub.rooms,
    );
    gauge(
        "lightleaf_ws_clients",
        "Connected editor WebSockets.",
        hub.clients,
    );
    gauge(
        "lightleaf_docs_loaded",
        "Documents held in memory.",
        hub.docs,
    );
    gauge(
        "lightleaf_compiles_running",
        "Compiles holding a compile slot.",
        running as u64,
    );
    gauge(
        "lightleaf_compile_slots",
        "LIGHTLEAF_MAX_COMPILES.",
        state.cfg.max_concurrent_compiles as u64,
    );
    if let Some(rss) = rss_bytes() {
        gauge(
            "lightleaf_resident_memory_bytes",
            "Resident memory of the server process.",
            rss,
        );
    }

    let c = |v: &AtomicU64| v.load(Relaxed);
    let _ = writeln!(
        out,
        "# HELP lightleaf_compiles_total Compiles run, by result.\n# TYPE lightleaf_compiles_total counter"
    );
    for (status, v) in [
        ("success", &m.compiles_success),
        ("errors", &m.compiles_errors),
        ("failure", &m.compiles_failure),
        ("timeout", &m.compiles_timeout),
    ] {
        let _ = writeln!(
            out,
            "lightleaf_compiles_total{{status=\"{status}\"}} {}",
            c(v)
        );
    }
    let mut counter = |name: &str, help: &str, v: u64| {
        let _ = writeln!(
            out,
            "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}"
        );
    };
    counter(
        "lightleaf_compile_seconds_total",
        "Time spent compiling.",
        c(&m.compile_millis) / 1000,
    );
    counter(
        "lightleaf_compiles_coalesced_total",
        "Compile requests answered by a concurrent compile.",
        c(&m.compiles_coalesced),
    );
    counter(
        "lightleaf_compiles_rejected_total",
        "Compile requests rejected by per-user limits.",
        c(&m.compiles_rejected),
    );
    counter(
        "lightleaf_logins_total",
        "Successful logins.",
        c(&m.logins_ok),
    );
    counter(
        "lightleaf_logins_failed_total",
        "Failed logins.",
        c(&m.logins_failed),
    );
    counter(
        "lightleaf_auth_rate_limited_total",
        "Authentication attempts rejected by rate limits.",
        c(&m.auth_rate_limited),
    );
    counter(
        "lightleaf_ws_connections_total",
        "Editor WebSocket connections opened.",
        c(&m.ws_connections),
    );
    counter(
        "lightleaf_ws_slow_disconnects_total",
        "Clients disconnected because they fell behind.",
        c(&m.ws_slow_disconnects),
    );
    counter(
        "lightleaf_ws_updates_total",
        "Document updates received.",
        c(&m.ws_updates),
    );

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        out,
    )
        .into_response()
}
