//! Semantic Scholar Graph API client: DOI batch lookups and title search, with a process-wide
//! rate limiter, retries honouring `Retry-After`, and a lookup cache with expiry.

use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const FIELDS: &str = "title,authors,year,venue,externalIds,publicationVenue";
const CACHE_TTL: Duration = Duration::from_secs(7 * 86_400);

#[derive(Clone, Debug)]
pub struct Settings {
    pub base_url: String,
    pub api_key: Option<String>,
    /// Minimum time between requests (process-wide).
    pub min_interval: Duration,
    pub max_retries: u32,
    pub timeout: Duration,
}

/// A lookup outcome. `Unavailable` (rate limits, outages) never becomes a "fabricated" verdict.
#[derive(Debug, Clone)]
pub enum Lookup<T> {
    Found(T),
    NotFound,
    Unavailable,
}

/// Process-wide FIFO limiter: one request per interval; `Retry-After` pauses everyone.
fn next_slot() -> &'static Mutex<Instant> {
    static NEXT: OnceLock<Mutex<Instant>> = OnceLock::new();
    NEXT.get_or_init(|| Mutex::new(Instant::now()))
}

async fn acquire(interval: Duration) {
    let wait = {
        let mut next = next_slot().lock().unwrap();
        let now = Instant::now();
        let slot = (*next).max(now);
        *next = slot + interval;
        slot - now
    };
    tokio::time::sleep(wait).await;
}

fn pause_all(d: Duration) {
    let mut next = next_slot().lock().unwrap();
    let target = Instant::now() + d;
    if target > *next {
        *next = target;
    }
}

fn cache() -> &'static Mutex<HashMap<String, (Instant, Value)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, Value)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn cached(key: &str) -> Option<Value> {
    let c = cache().lock().unwrap();
    c.get(key)
        .filter(|(t, _)| t.elapsed() < CACHE_TTL)
        .map(|(_, v)| v.clone())
}

fn remember(key: String, v: Value) {
    let mut c = cache().lock().unwrap();
    if c.len() > 20_000 {
        c.retain(|_, (t, _)| t.elapsed() < CACHE_TTL);
    }
    c.insert(key, (Instant::now(), v));
}

pub struct Client {
    http: reqwest::Client,
    pub settings: Settings,
}

/// What one request produced.
enum Response {
    Ok(Value),
    NotFound,
    /// Worth retrying later (429, 5xx, network).
    Transient(String),
}

impl Client {
    pub fn new(settings: Settings) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(settings.timeout)
            .user_agent(concat!(
                "lightleaf-reviewer/",
                env!("CARGO_PKG_VERSION"),
                " (reference checker)"
            ))
            .build()?;
        Ok(Client { http, settings })
    }

    /// One API call with rate limiting and retries. Authentication errors and other 4xx are
    /// errors: they would otherwise make every reference look fabricated.
    async fn call(&self, req: impl Fn() -> reqwest::RequestBuilder) -> anyhow::Result<Response> {
        let mut last = String::new();
        for attempt in 0..=self.settings.max_retries {
            acquire(self.settings.min_interval).await;
            let mut r = req();
            if let Some(k) = &self.settings.api_key {
                r = r.header("x-api-key", k);
            }
            match r.send().await {
                Ok(res) => {
                    let status = res.status().as_u16();
                    let retry_after = res
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok());
                    match status {
                        200..=299 => match res.json::<Value>().await {
                            Ok(v) => return Ok(Response::Ok(v)),
                            Err(e) => last = format!("invalid JSON: {e}"),
                        },
                        404 => return Ok(Response::NotFound),
                        401 | 403 => anyhow::bail!(
                            "Semantic Scholar rejected the API key ({status}); check LIGHTLEAF_S2_API_KEY"
                        ),
                        429 | 500..=599 => {
                            last = format!("Semantic Scholar returned {status}");
                            if let Some(s) = retry_after {
                                pause_all(Duration::from_secs(s.min(120)));
                            }
                        }
                        _ => {
                            let text: String = res
                                .text()
                                .await
                                .unwrap_or_default()
                                .chars()
                                .take(200)
                                .collect();
                            anyhow::bail!("Semantic Scholar returned {status}: {text}");
                        }
                    }
                }
                Err(e) => last = format!("request failed: {e}"),
            }
            if attempt < self.settings.max_retries {
                // Full jitter: random in [0, min(30 s, 1 s * 2^attempt)).
                let cap = 30_000u64.min(1000 * 2u64.pow(attempt.min(10)));
                let jitter = (uuid::Uuid::new_v4().as_u128() % u128::from(cap.max(1))) as u64;
                tokio::time::sleep(Duration::from_millis(jitter)).await;
            }
        }
        Ok(Response::Transient(last))
    }

    /// Looks up papers by DOI (normalised, lowercase), 100 per request.
    pub async fn by_dois(&self, dois: &[String]) -> anyhow::Result<HashMap<String, Lookup<Value>>> {
        let mut out = HashMap::new();
        let mut todo = Vec::new();
        for d in dois {
            match cached(&format!("doi:{d}")) {
                Some(Value::Null) => {
                    out.insert(d.clone(), Lookup::NotFound);
                }
                Some(v) => {
                    out.insert(d.clone(), Lookup::Found(v));
                }
                None => todo.push(d.clone()),
            }
        }
        for chunk in todo.chunks(100) {
            let url = format!("{}/paper/batch?fields={FIELDS}", self.settings.base_url);
            let body =
                json!({ "ids": chunk.iter().map(|d| format!("DOI:{d}")).collect::<Vec<_>>() });
            match self.call(|| self.http.post(&url).json(&body)).await? {
                Response::Ok(Value::Array(papers)) => {
                    for (d, p) in chunk.iter().zip(papers) {
                        remember(format!("doi:{d}"), p.clone());
                        out.insert(
                            d.clone(),
                            if p.is_null() {
                                Lookup::NotFound
                            } else {
                                Lookup::Found(p)
                            },
                        );
                    }
                }
                Response::Ok(_) | Response::NotFound => {
                    for d in chunk {
                        out.insert(d.clone(), Lookup::NotFound);
                    }
                }
                Response::Transient(e) => {
                    tracing::warn!("DOI lookup unavailable: {e}");
                    for d in chunk {
                        out.insert(d.clone(), Lookup::Unavailable);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Up to 5 candidate papers for a title.
    pub async fn search(&self, title_query: &str) -> anyhow::Result<Lookup<Vec<Value>>> {
        let q: String = title_query.chars().take(300).collect();
        let key = format!("q:{q}");
        if let Some(v) = cached(&key) {
            return Ok(Lookup::Found(v.as_array().cloned().unwrap_or_default()));
        }
        let url = format!("{}/paper/search", self.settings.base_url);
        let res = self
            .call(|| {
                self.http.get(&url).query(&[
                    ("query", q.as_str()),
                    ("limit", "5"),
                    ("fields", FIELDS),
                ])
            })
            .await?;
        Ok(match res {
            Response::Ok(v) => {
                let list = v["data"].as_array().cloned().unwrap_or_default();
                remember(key, Value::Array(list.clone()));
                Lookup::Found(list)
            }
            Response::NotFound => Lookup::Found(Vec::new()),
            Response::Transient(e) => {
                tracing::warn!("title search unavailable: {e}");
                Lookup::Unavailable
            }
        })
    }
}
