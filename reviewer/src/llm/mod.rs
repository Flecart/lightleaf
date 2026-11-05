//! Model access for the reviewer.
//!
//! [`Llm`] owns the HTTP client and the retry loop (the parts every provider shares). A
//! [`Provider`] only knows how to turn a [`Request`] into that provider's HTTP call and how to
//! read its answer. Adding a provider means implementing [`Provider`]; nothing else changes.

mod anthropic;
mod openai;

pub use anthropic::Anthropic;
pub use openai::OpenAi;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Duration,
};

/// One structured-output call: a system prompt shared by all calls of a job, a large shared
/// document (sent first, so provider-side prompt caching can reuse it across calls), the
/// call-specific task, and the JSON schema the answer must follow.
pub struct Request<'a> {
    pub system: &'a str,
    pub document: &'a str,
    pub task: &'a str,
    pub schema: &'a Value,
    pub max_tokens: u32,
}

/// Token counts of one response.
#[derive(Default, Debug, PartialEq)]
pub struct Tokens {
    /// Uncached input tokens.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// Token usage accumulated over a job (shown in the Review dialog).
#[derive(Default)]
pub struct Usage {
    input: AtomicU64,
    output: AtomicU64,
    cache_read: AtomicU64,
    cache_write: AtomicU64,
    requests: AtomicU64,
}

impl Usage {
    pub fn add(&self, t: &Tokens) {
        self.input.fetch_add(t.input, Relaxed);
        self.output.fetch_add(t.output, Relaxed);
        self.cache_read.fetch_add(t.cache_read, Relaxed);
        self.cache_write.fetch_add(t.cache_write, Relaxed);
        self.requests.fetch_add(1, Relaxed);
    }

    pub fn to_json(&self) -> Value {
        json!({
            "inputTokens": self.input.load(Relaxed),
            "outputTokens": self.output.load(Relaxed),
            "cacheReadTokens": self.cache_read.load(Relaxed),
            "cacheWriteTokens": self.cache_write.load(Relaxed),
            "requests": self.requests.load(Relaxed),
        })
    }
}

/// What a provider must supply: request construction and response interpretation.
pub trait Provider: Send + Sync {
    /// Short name used in logs and job results (`anthropic`, `openai`).
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;
    /// Builds the HTTP request for `r`.
    fn build(&self, http: &reqwest::Client, r: &Request) -> reqwest::RequestBuilder;
    /// Reads a 200 response: the structured output and its token counts. Refusals and
    /// truncated answers must be errors, never partial results.
    fn parse(&self, body: &Value) -> anyhow::Result<(Value, Tokens)>;
    /// Whether an HTTP status is worth retrying (rate limits, overload, server errors).
    fn retryable(&self, status: u16) -> bool {
        matches!(status, 408 | 409 | 429 | 500 | 502 | 503 | 504 | 529)
    }
}

/// Retry policy shared by all providers.
#[derive(Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    /// Upper bound for a server-requested `retry-after`.
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 5,
            max_wait: Duration::from_secs(120),
        }
    }
}

impl RetryPolicy {
    /// Delay before retry `attempt` (1-based): the server's `retry-after`, else exponential.
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        retry_after
            .map(|d| d.min(self.max_wait))
            .unwrap_or_else(|| Duration::from_secs(2u64.pow(attempt.min(6))))
    }
}

/// A model client: one provider plus the shared transport and retry loop.
pub struct Llm {
    http: reqwest::Client,
    provider: Box<dyn Provider>,
    retry: RetryPolicy,
}

impl Llm {
    pub fn new(provider: Box<dyn Provider>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("lightleaf-reviewer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Llm {
            http,
            provider,
            retry: RetryPolicy::default(),
        })
    }

    pub fn provider(&self) -> &'static str {
        self.provider.name()
    }

    pub fn model(&self) -> &str {
        self.provider.model()
    }

    /// Sends the request (retrying transient failures) and returns the JSON the model produced.
    pub async fn json(&self, r: &Request<'_>, usage: &Usage) -> anyhow::Result<Value> {
        let name = self.provider.name();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let last = attempt >= self.retry.max_attempts;
            let res = match self.provider.build(&self.http, r).send().await {
                Ok(res) => res,
                Err(e) if !last && (e.is_timeout() || e.is_connect() || e.is_request()) => {
                    tracing::warn!("{name} API connection error (attempt {attempt}): {e}");
                    tokio::time::sleep(self.retry.delay(attempt, None)).await;
                    continue;
                }
                Err(e) => return Err(e).with_context(|| format!("calling the {name} API")),
            };
            let status = res.status().as_u16();
            let retry_after = res
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<f64>().ok())
                .map(Duration::from_secs_f64);
            let body: Value = res.json().await.unwrap_or(Value::Null);
            if status == 200 {
                let (out, tokens) = self.provider.parse(&body)?;
                usage.add(&tokens);
                return Ok(out);
            }
            if !last && self.provider.retryable(status) {
                tracing::warn!("{name} API returned {status} (attempt {attempt}), retrying");
                tokio::time::sleep(self.retry.delay(attempt, retry_after)).await;
                continue;
            }
            let msg = body["error"]["message"].as_str().unwrap_or("no message");
            bail!("{name} API returned {status}: {msg}");
        }
    }
}

/// Parses a JSON object produced as text by a model.
fn parse_json_text(text: &str) -> anyhow::Result<Value> {
    serde_json::from_str(text.trim()).context("structured output was not valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_honours_server_and_caps_it() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(3))),
            Duration::from_secs(3)
        );
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(999))),
            Duration::from_secs(120)
        );
        assert_eq!(p.delay(3, None), Duration::from_secs(8));
    }

    #[test]
    fn usage_accumulates() {
        let u = Usage::default();
        u.add(&Tokens {
            input: 10,
            output: 2,
            cache_read: 5,
            cache_write: 1,
        });
        u.add(&Tokens {
            input: 1,
            ..Default::default()
        });
        assert_eq!(u.to_json()["inputTokens"], 11);
        assert_eq!(u.to_json()["requests"], 2);
    }
}
