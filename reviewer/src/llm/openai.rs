//! OpenAI-compatible provider: `POST {base}/chat/completions` with a strict JSON-schema
//! `response_format`. Works with OpenAI and compatible servers (OpenRouter, LiteLLM, vLLM,
//! Ollama, ...). OpenAI caches long identical prefixes automatically, so the shared paper
//! comes first in the user message.

use super::{Provider, Request, Tokens, parse_json_text};
use anyhow::{anyhow, bail};
use serde_json::{Value, json};

pub struct OpenAi {
    pub base_url: String,
    /// Empty for local servers that need no key.
    pub api_key: String,
    pub model: String,
    /// `reasoning_effort` for reasoning models; `None` = not sent (other models reject it).
    pub reasoning_effort: Option<String>,
    /// `max_completion_tokens` (OpenAI) or `max_tokens` (older compatible servers).
    pub max_tokens_field: String,
}

impl OpenAi {
    fn body(&self, r: &Request) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": r.system },
                { "role": "user", "content": if r.document.is_empty() { r.task.to_string() } else { format!("{}\n\n{}", r.document, r.task) } },
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
}

impl Provider for OpenAi {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn build(&self, http: &reqwest::Client, r: &Request) -> reqwest::RequestBuilder {
        let req = http
            .post(format!(
                "{}/chat/completions",
                self.base_url.trim_end_matches('/')
            ))
            .json(&self.body(r));
        if self.api_key.is_empty() {
            req
        } else {
            req.bearer_auth(&self.api_key)
        }
    }

    fn parse(&self, v: &Value) -> anyhow::Result<(Value, Tokens)> {
        let choice = &v["choices"][0];
        let msg = &choice["message"];
        if let Some(refusal) = msg["refusal"].as_str() {
            bail!("the model declined this request: {refusal}");
        }
        match choice["finish_reason"].as_str() {
            Some("length") => bail!("the model's answer was cut off (output length limit reached)"),
            Some("content_filter") => {
                bail!("the answer was blocked by the provider's content filter")
            }
            _ => {}
        }
        let text = msg["content"]
            .as_str()
            .ok_or_else(|| anyhow!("response contained no content"))?;
        let u = &v["usage"];
        let n = |x: &Value| x.as_u64().unwrap_or(0);
        let cached = n(&u["prompt_tokens_details"]["cached_tokens"]);
        let tokens = Tokens {
            input: n(&u["prompt_tokens"]).saturating_sub(cached),
            output: n(&u["completion_tokens"]),
            cache_read: cached,
            cache_write: 0,
        };
        Ok((parse_json_text(text)?, tokens))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> OpenAi {
        OpenAi {
            base_url: "https://api.openai.com/v1/".into(),
            api_key: "k".into(),
            model: "m".into(),
            reasoning_effort: None,
            max_tokens_field: "max_completion_tokens".into(),
        }
    }

    #[test]
    fn parses_completions_and_failures() {
        let p = provider();
        let ok = json!({"usage": {"prompt_tokens": 10, "prompt_tokens_details": {"cached_tokens": 8}},
            "choices": [{"finish_reason": "stop", "message": {"content": "{\"comments\": []}", "refusal": null}}]});
        let (out, t) = p.parse(&ok).unwrap();
        assert_eq!(out, json!({"comments": []}));
        assert_eq!((t.input, t.cache_read), (2, 8));
        assert!(
            p.parse(&json!({"choices": [{"message": {"content": null, "refusal": "no"}}]}))
                .is_err()
        );
        assert!(
            p.parse(
                &json!({"choices": [{"finish_reason": "length", "message": {"content": "{"}}]})
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
        assert_eq!(b["response_format"]["json_schema"]["strict"], true);
        assert_eq!(b["max_completion_tokens"], 100);
        assert_eq!(b["messages"][1]["content"], "d\n\nt");
        assert!(b.get("reasoning_effort").is_none());
    }
}
