//! Request-level hardening: client IP detection, "is this request from the server itself",
//! login rate limiting, cross-site request checks and security headers.

use crate::state::SharedState;
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Mutex,
    time::{Duration, Instant},
};

/// Headers that reveal a request was relayed by a reverse proxy.
const PROXY_HEADERS: [&str; 5] = [
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
];

/// The address of the connected peer (IPv4-mapped IPv6 addresses are unwrapped).
pub fn peer(ci: &ConnectInfo<SocketAddr>) -> IpAddr {
    ci.0.ip().to_canonical()
}

/// The client's IP address. Proxy headers are only honoured when `LIGHTLEAF_TRUST_PROXY` is set,
/// and then only the right-most `X-Forwarded-For` entry (the one appended by our own proxy) is used.
pub fn client_ip(trust_proxy: bool, peer: Option<IpAddr>, headers: &HeaderMap) -> String {
    if trust_proxy
        && let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .map(str::trim)
            .and_then(|v| v.parse::<IpAddr>().ok())
    {
        return ip.to_canonical().to_string();
    }
    peer.map(|p| p.to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn host_is_loopback(hostport: &str) -> bool {
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.to_canonical().is_loopback())
            .unwrap_or(false)
}

/// True only for a request made directly on the server machine: a loopback peer, no reverse-proxy
/// headers, and a loopback `Host` (and `Origin`, when the browser sends one).
///
/// This is what `LIGHTLEAF_SIGNUP=local` relies on. A reverse proxy running on the same machine
/// must add `X-Forwarded-For` (Caddy and Traefik always do), otherwise every proxied request
/// would look local.
pub fn is_local_request(peer: Option<IpAddr>, headers: &HeaderMap) -> bool {
    let Some(peer) = peer else { return false };
    if !peer.is_loopback() {
        return false;
    }
    if PROXY_HEADERS.iter().any(|h| headers.contains_key(*h)) {
        return false;
    }
    let host_ok = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(host_is_loopback);
    let origin_ok = match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(o) => host_is_loopback(o.split("://").nth(1).unwrap_or(o)),
    };
    host_ok && origin_ok
}

/// True when `Origin` (if any) matches the host the request was sent to.
pub fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let origin_host = origin.split("://").nth(1).unwrap_or(origin);
    let forwarded = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok());
    origin_host.eq_ignore_ascii_case(host)
        || forwarded.is_some_and(|f| origin_host.eq_ignore_ascii_case(f))
}

/// Rejects state-changing API requests that come from another site (defence in depth on top of
/// `SameSite=Lax` cookies and JSON-only bodies).
pub async fn csrf_guard(req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe {
        let headers = req.headers();
        let cross_site = headers
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == "cross-site");
        if cross_site || !same_origin(headers) {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({ "error": "cross-site request rejected" })),
            )
                .into_response();
        }
    }
    next.run(req).await
}

/// Adds HSTS when the deployment is HTTPS-only.
pub async fn hsts(State(state): State<SharedState>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    if state.cfg.secure_cookies {
        res.headers_mut().insert(
            header::STRICT_TRANSPORT_SECURITY,
            header::HeaderValue::from_static("max-age=31536000"),
        );
    }
    res
}

/// A fixed-window counter keyed by strings (IP addresses, email addresses).
pub struct RateLimiter {
    max: u32,
    window: Duration,
    hits: Mutex<HashMap<String, (u32, Instant)>>,
}

impl RateLimiter {
    pub fn new(max: u32, window: Duration) -> Self {
        RateLimiter {
            max,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Seconds until `key` may try again, or `None` if it is under the limit.
    pub fn blocked(&self, key: &str) -> Option<u64> {
        let hits = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        let (n, start) = hits.get(key)?;
        let elapsed = start.elapsed();
        (*n >= self.max && elapsed < self.window).then(|| (self.window - elapsed).as_secs().max(1))
    }

    /// Records one hit for `key`.
    pub fn hit(&self, key: &str) {
        let mut hits = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        if hits.len() > 50_000 {
            let window = self.window;
            hits.retain(|_, (_, start)| start.elapsed() < window);
        }
        let entry = hits.entry(key.to_string()).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= self.window {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    pub fn clear(&self, key: &str) {
        self.hits
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(key);
    }
}

/// Rate limiters for the unauthenticated endpoints.
pub struct Limits {
    /// Failed logins and all registrations per client IP.
    pub auth_per_ip: RateLimiter,
    /// Failed logins per account.
    pub failed_per_account: RateLimiter,
    /// Compiles actually run (not coalesced) per user and minute.
    pub compiles_per_user: RateLimiter,
    /// Compiles running or queued per user, capped at `max_user_compiles`.
    user_compiles: Mutex<HashMap<i64, usize>>,
    max_user_compiles: usize,
}

impl Limits {
    pub fn new(cfg: &crate::config::Config) -> Self {
        Limits {
            auth_per_ip: RateLimiter::new(30, Duration::from_secs(10 * 60)),
            failed_per_account: RateLimiter::new(10, Duration::from_secs(15 * 60)),
            compiles_per_user: RateLimiter::new(cfg.compiles_per_minute, Duration::from_secs(60)),
            user_compiles: Mutex::new(HashMap::new()),
            max_user_compiles: cfg.compiles_per_user,
        }
    }

    /// Reserves one of the user's concurrent compile slots, released when the guard drops.
    pub fn user_compile_slot(&self, user_id: i64) -> Option<UserCompileGuard<'_>> {
        let mut m = self.user_compiles.lock().unwrap_or_else(|p| p.into_inner());
        let n = m.entry(user_id).or_insert(0);
        if *n >= self.max_user_compiles {
            return None;
        }
        *n += 1;
        Some(UserCompileGuard {
            limits: self,
            user_id,
        })
    }
}

pub struct UserCompileGuard<'a> {
    limits: &'a Limits,
    user_id: i64,
}

impl Drop for UserCompileGuard<'_> {
    fn drop(&mut self) {
        let mut m = self
            .limits
            .user_compiles
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(n) = m.get_mut(&self.user_id) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.user_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn local_detection() {
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        let lo6: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        let remote: IpAddr = "203.0.113.5".parse().unwrap();
        let h = headers(&[("host", "localhost:8080")]);
        assert!(is_local_request(Some(lo), &h));
        assert!(is_local_request(Some(lo6.to_canonical()), &h));
        assert!(is_local_request(
            Some(lo),
            &headers(&[("host", "[::1]:8080")])
        ));
        assert!(!is_local_request(Some(remote), &h));
        assert!(!is_local_request(None, &h));
        assert!(!is_local_request(
            Some(lo),
            &headers(&[("host", "latex.example.com")])
        ));
        assert!(!is_local_request(
            Some(lo),
            &headers(&[
                ("host", "localhost:8080"),
                ("x-forwarded-for", "203.0.113.5")
            ])
        ));
        assert!(!is_local_request(
            Some(lo),
            &headers(&[
                ("host", "localhost:8080"),
                ("origin", "https://evil.example")
            ])
        ));
        assert!(is_local_request(
            Some(lo),
            &headers(&[
                ("host", "127.0.0.1:8080"),
                ("origin", "http://127.0.0.1:8080")
            ])
        ));
    }

    #[test]
    fn client_ip_uses_proxy_only_when_trusted() {
        let peer: IpAddr = "172.18.0.2".parse().unwrap();
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 198.51.100.7")]);
        assert_eq!(client_ip(false, Some(peer), &h), "172.18.0.2");
        assert_eq!(client_ip(true, Some(peer), &h), "198.51.100.7");
        assert_eq!(client_ip(true, Some(peer), &HeaderMap::new()), "172.18.0.2");
    }

    #[test]
    fn rate_limiter() {
        let l = RateLimiter::new(2, Duration::from_secs(60));
        assert!(l.blocked("a").is_none());
        l.hit("a");
        l.hit("a");
        assert!(l.blocked("a").is_some());
        assert!(l.blocked("b").is_none());
        l.clear("a");
        assert!(l.blocked("a").is_none());
    }

    #[test]
    fn user_compile_slots() {
        let mut cfg = crate::config::Config::from_env();
        cfg.compiles_per_user = 1;
        let l = Limits::new(&cfg);
        let g = l.user_compile_slot(7);
        assert!(g.is_some());
        assert!(l.user_compile_slot(7).is_none());
        assert!(l.user_compile_slot(8).is_some());
        drop(g);
        assert!(l.user_compile_slot(7).is_some());
    }

    #[test]
    fn origin_check() {
        assert!(same_origin(&headers(&[("host", "a.example")])));
        assert!(same_origin(&headers(&[
            ("host", "a.example"),
            ("origin", "https://a.example")
        ])));
        assert!(!same_origin(&headers(&[
            ("host", "a.example"),
            ("origin", "https://b.example")
        ])));
    }
}
