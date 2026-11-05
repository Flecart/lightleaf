//! Reviewer configuration, from environment variables.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    /// Shared secret Lightleaf sends as `Authorization: Bearer …`.
    pub secret: String,
    /// `anthropic` or `openai` (any OpenAI-compatible Chat Completions API).
    pub provider: String,
    pub anthropic_api_key: Option<String>,
    pub openai_api_key: Option<String>,
    pub openai_base_url: String,
    /// Explicitly configured OpenAI-compatible endpoint (local servers may need no key).
    pub openai_base_url_set: bool,
    pub openai_reasoning_effort: Option<String>,
    pub openai_max_tokens_field: String,
    pub anthropic_base_url: String,
    pub model: String,
    pub effort: String,
    /// Send `fallbacks: "default"` (disable for gateways that do not support it).
    pub fallbacks: bool,
    /// Name of the comment macro (`\AI` by default); reserved for review comments.
    pub macro_name: String,
    pub max_comments: usize,
    pub max_jobs: usize,
    pub agent_concurrency: usize,
    pub agent_timeout_secs: u64,
    pub max_input_chars: usize,
    pub disabled_agents: Vec<String>,
    /// Pack file overrides: files at the same relative path replace the built-in ones.
    pub prompts_dir: Option<PathBuf>,
    /// Review methodology (`papermentor` or `lightleaf`).
    pub pack: String,
    /// Semantic Scholar access for the reference checker.
    pub s2: crate::references::s2::Settings,
    pub references_max_entries: usize,
    /// Default for strict mode when a request does not say.
    pub strict_default: bool,
    pub skip_anonymity: bool,
    pub skip_source_comments: bool,
    pub content_focus: bool,
    pub work_dir: PathBuf,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn flag(key: &str) -> bool {
    matches!(
        env(key).map(|v| v.to_ascii_lowercase()).as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn num<T: std::str::FromStr + PartialOrd + Default>(key: &str, default: T) -> T {
    env(key)
        .and_then(|v| v.parse().ok())
        .filter(|n: &T| *n > T::default())
        .unwrap_or(default)
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let secret = env("LIGHTLEAF_REVIEWER_SECRET")
            .filter(|s| s.len() >= 16)
            .ok_or_else(|| anyhow::anyhow!("LIGHTLEAF_REVIEWER_SECRET (16+ characters, same value as in Lightleaf) is required"))?;
        let macro_name = env("LIGHTLEAF_REVIEW_MACRO").unwrap_or_else(|| "AI".into());
        anyhow::ensure!(
            !macro_name.is_empty() && macro_name.chars().all(|c| c.is_ascii_alphabetic()),
            "LIGHTLEAF_REVIEW_MACRO must contain only letters (it becomes a LaTeX command name)"
        );
        let pack =
            env("LIGHTLEAF_REVIEW_PACK").unwrap_or_else(|| crate::review::packs::DEFAULT.into());
        anyhow::ensure!(
            crate::review::packs::ids().contains(&pack.as_str()),
            "LIGHTLEAF_REVIEW_PACK must be one of {:?}",
            crate::review::packs::ids()
        );
        let anthropic_api_key = env("ANTHROPIC_API_KEY");
        let openai_api_key = env("OPENAI_API_KEY");
        let openai_base = env("OPENAI_BASE_URL");
        let provider = match env("LIGHTLEAF_REVIEW_PROVIDER").map(|p| p.to_ascii_lowercase()) {
            Some(p) if p == "anthropic" || p == "openai" => p,
            Some(p) => anyhow::bail!(
                "LIGHTLEAF_REVIEW_PROVIDER must be \"anthropic\" or \"openai\", not {p:?}"
            ),
            // Without an explicit choice, use whichever provider has credentials.
            None if anthropic_api_key.is_none()
                && (openai_api_key.is_some() || openai_base.is_some()) =>
            {
                "openai".into()
            }
            None => "anthropic".into(),
        };
        let default_model = if provider == "openai" {
            "gpt-5.2-chat-latest"
        } else {
            "claude-opus-5-5"
        };
        Ok(Config {
            provider,
            openai_base_url_set: openai_base.is_some(),
            openai_base_url: openai_base.unwrap_or_else(|| "https://api.openai.com/v1".into()),
            openai_api_key,
            openai_reasoning_effort: env("LIGHTLEAF_REVIEW_OPENAI_REASONING_EFFORT"),
            openai_max_tokens_field: env("LIGHTLEAF_REVIEW_OPENAI_MAX_TOKENS_FIELD")
                .unwrap_or_else(|| "max_completion_tokens".into()),
            bind: env("LIGHTLEAF_REVIEWER_BIND").unwrap_or_else(|| "127.0.0.1:8090".into()),
            secret,
            anthropic_api_key,
            anthropic_base_url: env("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|| "https://api.anthropic.com".into()),
            model: env("LIGHTLEAF_REVIEW_MODEL").unwrap_or_else(|| default_model.into()),
            effort: env("LIGHTLEAF_REVIEW_EFFORT").unwrap_or_else(|| "high".into()),
            fallbacks: !matches!(
                env("LIGHTLEAF_REVIEW_FALLBACKS").as_deref(),
                Some("false") | Some("0")
            ),
            macro_name,
            max_comments: num("LIGHTLEAF_REVIEW_MAX_COMMENTS", 40),
            max_jobs: num("LIGHTLEAF_REVIEW_MAX_JOBS", 2),
            agent_concurrency: num("LIGHTLEAF_REVIEW_AGENT_CONCURRENCY", 6),
            agent_timeout_secs: num("LIGHTLEAF_REVIEW_AGENT_TIMEOUT", 600),
            max_input_chars: num("LIGHTLEAF_REVIEW_MAX_INPUT_CHARS", 800_000),
            disabled_agents: env("LIGHTLEAF_REVIEW_DISABLED_AGENTS")
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            prompts_dir: env("LIGHTLEAF_REVIEW_PROMPTS_DIR").map(PathBuf::from),
            pack,
            s2: {
                let api_key =
                    env("LIGHTLEAF_S2_API_KEY").or_else(|| env("SEMANTIC_SCHOLAR_API_KEY"));
                // Without a key, requests share Semantic Scholar's public pool: go slower.
                let default_interval = if api_key.is_some() { 1100 } else { 3500 };
                crate::references::s2::Settings {
                    base_url: env("LIGHTLEAF_S2_BASE_URL")
                        .unwrap_or_else(|| "https://api.semanticscholar.org/graph/v1".into()),
                    min_interval: std::time::Duration::from_millis(num(
                        "LIGHTLEAF_S2_MIN_INTERVAL_MS",
                        default_interval,
                    )),
                    max_retries: num("LIGHTLEAF_S2_MAX_RETRIES", 5),
                    timeout: std::time::Duration::from_millis(num(
                        "LIGHTLEAF_S2_TIMEOUT_MS",
                        30_000,
                    )),
                    api_key,
                }
            },
            references_max_entries: num("LIGHTLEAF_REFERENCES_MAX_ENTRIES", 200),
            strict_default: flag("LIGHTLEAF_REVIEW_STRICT"),
            skip_anonymity: flag("LIGHTLEAF_REVIEW_SKIP_ANONYMITY"),
            skip_source_comments: flag("LIGHTLEAF_REVIEW_SKIP_SOURCE_COMMENTS"),
            content_focus: flag("LIGHTLEAF_REVIEW_CONTENT_FOCUS"),
            work_dir: env("LIGHTLEAF_REVIEW_WORKDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("lightleaf-reviewer")),
        })
    }
}

impl Config {
    /// Whether the selected provider has what it needs to run reviews.
    pub fn llm_configured(&self) -> bool {
        match self.provider.as_str() {
            "openai" => self.openai_api_key.is_some() || self.openai_base_url_set,
            _ => self.anthropic_api_key.is_some(),
        }
    }

    /// Message shown when reviews are refused for lack of credentials.
    pub fn llm_missing_message(&self) -> &'static str {
        match self.provider.as_str() {
            "openai" => {
                "AI review is not configured on the server (OPENAI_API_KEY or OPENAI_BASE_URL is not set)"
            }
            _ => "AI review is not configured on the server (ANTHROPIC_API_KEY is not set)",
        }
    }
}
