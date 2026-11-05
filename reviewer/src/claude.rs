//! Minimal client for the Claude Messages API (raw HTTP; there is no official Rust SDK).
//!
//! Every call asks for structured JSON output (`output_config.format` with a JSON schema), so the
//! reviewer never parses free text. Retries 429/5xx/529 with `retry-after` or exponential backoff,
//! and treats refusals and truncated outputs as errors instead of reading partial content.

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Duration,
};

const API_VERSION: &str = "2026-10-21";
/// `fallbacks: "default"`: if a safety classifier declines a request, the API re-runs it on the
/// recommended fallback model instead of returning a refusal.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_ATTEMPTS: u32 = 5;

pub struct Claude {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    pub model: String,
    effort: String,
    fallbacks: bool,
}

/// Token usage accumulated over a job (for the status page and cost tracking).
#[derive(Default)]
pub struct Usage {
    pub input: AtomicU64,
    pub output: AtomicU64,
    pub cache_read: AtomicU64,
    pub cache_write: AtomicU64,
    pub requests: AtomicU64,
}

impl Usage {
    fn add(&self, u: &Value) {
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        self.input.fetch_add(n("input_tokens"), Relaxed);
        self.output.fetch_add(n("output_tokens"), Relaxed);
        self.cache_read
            .fetch_add(n("cache_read_input_tokens"), Relaxed);
        self.cache_write
            .fetch_add(n("cache_creation_input_tokens"), Relaxed);
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

/// One request: a system prompt shared by all calls of a job, a large shared document (cached
/// so parallel agents read it at a fraction of the cost), and the call-specific task.
pub struct Request<'a> {
    pub system: &'a str,
    pub document: &'a str,
    pub task: &'a str,
    pub schema: &'a Value,
    pub max_tokens: u32,
}

impl Claude {
    pub fn new(
        base_url: String,
        api_key: String,
        model: String,
        effort: String,
        fallbacks: bool,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("lightleaf-reviewer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Claude {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
            effort,
            fallbacks,
        })
    }

    fn body(&self, r: &Request) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": r.max_tokens,
            "system": [{ "type": "text", "text": r.system }],
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": r.document, "cache_control": { "type": "ephemeral" } },
                    { "type": "text", "text": r.task },
                ],
            }],
            "output_config": {
                "effort": self.effort,
                "format": { "type": "json_schema", "schema": r.schema },
            },
        });
        if self.fallbacks {
            body["fallbacks"] = json!("default");
        }
        body
    }

    /// Sends the request and returns the parsed JSON object the model produced.
    pub async fn json(&self, r: &Request<'_>, usage: &Usage) -> anyhow::Result<Value> {
        let body = self.body(r);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut req = self
                .http
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .json(&body);
            if self.fallbacks {
                req = req.header("anthropic-beta", FALLBACK_BETA);
            }
            let res = match req.send().await {
                Ok(res) => res,
                Err(e)
                    if attempt < MAX_ATTEMPTS
                        && (e.is_timeout() || e.is_connect() || e.is_request()) =>
                {
                    tracing::warn!("Claude API connection error (attempt {attempt}): {e}");
                    tokio::time::sleep(backoff(attempt, None)).await;
                    continue;
                }
                Err(e) => return Err(e).context("calling the Claude API"),
            };
            let status = res.status().as_u16();
            let retry_after = res
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let value: Value = res.json().await.unwrap_or(Value::Null);
            match status {
                200 => {
                    usage.add(&value["usage"]);
                    return parse_output(&value);
                }
                429 | 500 | 502 | 503 | 504 | 529 if attempt < MAX_ATTEMPTS => {
                    tracing::warn!("Claude API {status} (attempt {attempt}), retrying");
                    tokio::time::sleep(backoff(attempt, retry_after)).await;
                }
                _ => {
                    let kind = value["error"]["type"].as_str().unwrap_or("error");
                    let msg = value["error"]["message"].as_str().unwrap_or("no message");
                    bail!("Claude API returned {status} {kind}: {msg}");
                }
            }
        }
    }
}

fn backoff(attempt: u32, retry_after: Option<u64>) -> Duration {
    match retry_after {
        Some(s) => Duration::from_secs(s.min(120)),
        None => Duration::from_secs(2u64.pow(attempt.min(6))),
    }
}

/// Extracts the structured output from a Messages API response.
fn parse_output(v: &Value) -> anyhow::Result<Value> {
    match v["stop_reason"].as_str() {
        Some("refusal") => {
            let cat = v["stop_details"]["category"]
                .as_str()
                .unwrap_or("unspecified");
            bail!("the model declined this request (category: {cat})");
        }
        Some("max_tokens") => bail!("the model's answer was cut off (max_tokens reached)"),
        _ => {}
    }
    // With structured outputs the answer is the (last) text block; thinking blocks may precede it.
    let text = v["content"]
        .as_array()
        .and_then(|blocks| {
            blocks
                .iter()
                .rev()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
        })
        .ok_or_else(|| anyhow!("response contained no text block"))?;
    serde_json::from_str(text).context("structured output was not valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structured_output_and_refusals() {
        let ok = json!({"stop_reason": "end_turn", "content": [
            {"type": "thinking", "thinking": ""},
            {"type": "text", "text": "{\"comments\": []}"}
        ]});
        assert_eq!(parse_output(&ok).unwrap(), json!({"comments": []}));
        let refused =
            json!({"stop_reason": "refusal", "stop_details": {"category": "cyber"}, "content": []});
        assert!(
            parse_output(&refused)
                .unwrap_err()
                .to_string()
                .contains("cyber")
        );
        let cut = json!({"stop_reason": "max_tokens", "content": [{"type": "text", "text": "{"}]});
        assert!(parse_output(&cut).is_err());
    }

    #[test]
    fn request_body_shape() {
        let c = Claude::new(
            "https://api.anthropic.com".into(),
            "k".into(),
            "claude-opus-5-5".into(),
            "high".into(),
            true,
        )
        .unwrap();
        let schema = json!({"type": "object"});
        let b = c.body(&Request {
            system: "s",
            document: "d",
            task: "t",
            schema: &schema,
            max_tokens: 100,
        });
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(
            b["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(b["fallbacks"], "default");
        assert!(b.get("thinking").is_none() && b.get("temperature").is_none());
    }
}
