//! Anthropic provider: the Claude Messages API (raw HTTP; there is no official Rust SDK).
//! Structured output via `output_config.format`, the paper as a cached content block, and
//! server-side refusal fallbacks (`fallbacks: "default"`).

use super::{Provider, Request, Tokens, parse_json_text};
use anyhow::{anyhow, bail};
use serde_json::{Value, json};

const API_VERSION: &str = "2026-10-21";
/// Beta header for `fallbacks: "default"`: a declined request is re-run on the recommended
/// fallback model instead of returning a refusal.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

pub struct Anthropic {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// `output_config.effort`: low | medium | high | xhigh | max.
    pub effort: String,
    pub fallbacks: bool,
}

impl Anthropic {
    fn body(&self, r: &Request) -> Value {
        // The shared document (if any) is its own cached block, before the task.
        let mut content = Vec::new();
        if !r.document.is_empty() {
            content.push(json!({ "type": "text", "text": r.document, "cache_control": { "type": "ephemeral" } }));
        }
        content.push(json!({ "type": "text", "text": r.task }));
        let mut body = json!({
            "model": self.model,
            "max_tokens": r.max_tokens,
            "system": [{ "type": "text", "text": r.system }],
            "messages": [{ "role": "user", "content": content }],
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
}

impl Provider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn build(&self, http: &reqwest::Client, r: &Request) -> reqwest::RequestBuilder {
        let mut req = http
            .post(format!(
                "{}/v1/messages",
                self.base_url.trim_end_matches('/')
            ))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&self.body(r));
        if self.fallbacks {
            req = req.header("anthropic-beta", FALLBACK_BETA);
        }
        req
    }

    fn parse(&self, v: &Value) -> anyhow::Result<(Value, Tokens)> {
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
        // The answer is the last text block; thinking blocks may precede it.
        let text = v["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().rev().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .ok_or_else(|| anyhow!("response contained no text block"))?;
        let u = &v["usage"];
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        let tokens = Tokens {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
        };
        Ok((parse_json_text(text)?, tokens))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> Anthropic {
        Anthropic {
            base_url: "https://api.anthropic.com".into(),
            api_key: "k".into(),
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
            fallbacks: true,
        }
    }

    #[test]
    fn parses_structured_output_and_refusals() {
        let p = provider();
        let ok = json!({"stop_reason": "end_turn", "usage": {"input_tokens": 3, "cache_read_input_tokens": 7}, "content": [
            {"type": "thinking", "thinking": ""},
            {"type": "text", "text": "{\"comments\": []}"}
        ]});
        let (out, t) = p.parse(&ok).unwrap();
        assert_eq!(out, json!({"comments": []}));
        assert_eq!((t.input, t.cache_read), (3, 7));
        let refused =
            json!({"stop_reason": "refusal", "stop_details": {"category": "cyber"}, "content": []});
        assert!(p.parse(&refused).unwrap_err().to_string().contains("cyber"));
        assert!(
            p.parse(
                &json!({"stop_reason": "max_tokens", "content": [{"type": "text", "text": "{"}]})
            )
            .is_err()
        );
    }

    #[test]
    fn request_body_shape() {
        let schema = json!({"type": "object"});
        let b = provider().body(&Request {
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
