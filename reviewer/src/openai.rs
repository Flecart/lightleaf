//! OpenAI-compatible backend: `POST {base}/chat/completions` with `response_format` set to a
//! strict JSON schema. Works with OpenAI and with compatible servers (OpenRouter, LiteLLM,
//! vLLM, Ollama, ...). Prompt caching is automatic on OpenAI for long identical prefixes, which
//! is why the shared document comes first in the user message.

use crate::llm::{Request, Usage, backoff};
use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;

const MAX_ATTEMPTS: u32 = 5;

pub struct OpenAi {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    pub model: String,
    /// `reasoning_effort` for reasoning models; `None` = not sent (most models reject it).
    reasoning_effort: Option<String>,
    /// Name of the output-length field: `max_completion_tokens` (OpenAI) or `max_tokens`
    /// (older compatible servers).
    max_tokens_field: String,
}

impl OpenAi {
    pub fn new(
        base_url: String,
        api_key: String,
        model: String,
        reasoning_effort: Option<String>,
        max_tokens_field: String,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("lightleaf-reviewer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(OpenAi {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
            reasoning_effort,
            max_tokens_field,
        })
    }

    fn body(&self, r: &Request) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": r.system },
                { "role": "user", "content": format!("{}\n\n{}", r.document, r.task) },
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "review_output", "strict": true, "schema": r.schema },
            },
        });
        body[self.max_tokens_field.as_str()] = json!(r.max_tokens);
        if let Some(e) = &self.reasoning_effort {
            body["reasoning_effort"] = json!(e);
        }
        body
    }

    pub async fn json(&self, r: &Request<'_>, usage: &Usage) -> anyhow::Result<Value> {
        let body = self.body(r);
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut req = self
                .http
                .post(format!("{}/chat/completions", self.base_url))
                .json(&body);
            if !self.api_key.is_empty() {
                req = req.bearer_auth(&self.api_key);
            }
            let res = match req.send().await {
                Ok(res) => res,
                Err(e)
                    if attempt < MAX_ATTEMPTS
                        && (e.is_timeout() || e.is_connect() || e.is_request()) =>
                {
                    tracing::warn!("OpenAI API connection error (attempt {attempt}): {e}");
                    tokio::time::sleep(backoff(attempt, None)).await;
                    continue;
                }
                Err(e) => return Err(e).context("calling the OpenAI-compatible API"),
            };
            let status = res.status().as_u16();
            let retry_after = res
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok())
                .map(|s| s.ceil() as u64);
            let value: Value = res.json().await.unwrap_or(Value::Null);
            match status {
                200 => {
                    let u = &value["usage"];
                    let n = |v: &Value| v.as_u64().unwrap_or(0);
                    let cached = n(&u["prompt_tokens_details"]["cached_tokens"]);
                    usage.add(
                        n(&u["prompt_tokens"]).saturating_sub(cached),
                        n(&u["completion_tokens"]),
                        cached,
                        0,
                    );
                    return parse_output(&value);
                }
                408 | 409 | 429 | 500 | 502 | 503 | 504 if attempt < MAX_ATTEMPTS => {
                    tracing::warn!("OpenAI API {status} (attempt {attempt}), retrying");
                    tokio::time::sleep(backoff(attempt, retry_after)).await;
                }
                _ => {
                    let msg = value["error"]["message"].as_str().unwrap_or("no message");
                    bail!("OpenAI-compatible API returned {status}: {msg}");
                }
            }
        }
    }
}

fn parse_output(v: &Value) -> anyhow::Result<Value> {
    let choice = &v["choices"][0];
    let msg = &choice["message"];
    if let Some(refusal) = msg["refusal"].as_str() {
        bail!("the model declined this request: {refusal}");
    }
    match choice["finish_reason"].as_str() {
        Some("length") => bail!("the model's answer was cut off (output length limit reached)"),
        Some("content_filter") => bail!("the answer was blocked by the provider's content filter"),
        _ => {}
    }
    let text = msg["content"]
        .as_str()
        .ok_or_else(|| anyhow!("response contained no content"))?;
    serde_json::from_str(text.trim()).context("structured output was not valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_completions_and_failures() {
        let ok = json!({"choices": [{"finish_reason": "stop", "message": {"content": "{\"comments\": []}", "refusal": null}}]});
        assert_eq!(parse_output(&ok).unwrap(), json!({"comments": []}));
        let refused = json!({"choices": [{"finish_reason": "stop", "message": {"content": null, "refusal": "no"}}]});
        assert!(parse_output(&refused).is_err());
        let cut = json!({"choices": [{"finish_reason": "length", "message": {"content": "{"}}]});
        assert!(parse_output(&cut).is_err());
    }

    #[test]
    fn request_body_shape() {
        let o = OpenAi::new(
            "https://api.openai.com/v1/".into(),
            "k".into(),
            "m".into(),
            None,
            "max_completion_tokens".into(),
        )
        .unwrap();
        let schema = json!({"type": "object"});
        let b = o.body(&Request {
            system: "s",
            document: "d",
            task: "t",
            schema: &schema,
            max_tokens: 100,
        });
        assert_eq!(b["response_format"]["json_schema"]["strict"], true);
        assert_eq!(b["max_completion_tokens"], 100);
        assert_eq!(b["messages"][1]["content"], "d\n\nt");
        assert!(b.get("reasoning_effort").is_none());
        assert_eq!(o.base_url, "https://api.openai.com/v1");
    }
}
