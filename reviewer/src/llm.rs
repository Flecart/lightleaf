//! The model backend used by the reviewer: Anthropic (Claude Messages API, default) or any
//! OpenAI-compatible Chat Completions API (OpenAI, Azure-style gateways, OpenRouter, LiteLLM,
//! vLLM, Ollama, ...). Both return the structured JSON object requested by a JSON schema.

use crate::{claude::Claude, openai::OpenAi};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Duration,
};

/// One request: a system prompt shared by all calls of a job, a large shared document (placed
/// first so the provider's prompt cache can reuse it across agents), and the call's task.
pub struct Request<'a> {
    pub system: &'a str,
    pub document: &'a str,
    pub task: &'a str,
    pub schema: &'a Value,
    pub max_tokens: u32,
}

/// Token usage accumulated over a job (shown in the Review dialog).
#[derive(Default)]
pub struct Usage {
    pub input: AtomicU64,
    pub output: AtomicU64,
    pub cache_read: AtomicU64,
    pub cache_write: AtomicU64,
    pub requests: AtomicU64,
}

impl Usage {
    pub fn add(&self, input: u64, output: u64, cache_read: u64, cache_write: u64) {
        self.input.fetch_add(input, Relaxed);
        self.output.fetch_add(output, Relaxed);
        self.cache_read.fetch_add(cache_read, Relaxed);
        self.cache_write.fetch_add(cache_write, Relaxed);
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

/// Delay before retry `attempt` (1-based): the server's `retry-after`, else exponential.
pub fn backoff(attempt: u32, retry_after: Option<u64>) -> Duration {
    match retry_after {
        Some(s) => Duration::from_secs(s.min(120)),
        None => Duration::from_secs(2u64.pow(attempt.min(6))),
    }
}

pub enum Llm {
    Anthropic(Claude),
    OpenAi(OpenAi),
}

impl Llm {
    pub fn provider(&self) -> &'static str {
        match self {
            Llm::Anthropic(_) => "anthropic",
            Llm::OpenAi(_) => "openai",
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Llm::Anthropic(c) => &c.model,
            Llm::OpenAi(o) => &o.model,
        }
    }

    /// Sends the request and returns the JSON object the model produced.
    pub async fn json(&self, r: &Request<'_>, usage: &Usage) -> anyhow::Result<Value> {
        match self {
            Llm::Anthropic(c) => c.json(r, usage).await,
            Llm::OpenAi(o) => o.json(r, usage).await,
        }
    }
}
