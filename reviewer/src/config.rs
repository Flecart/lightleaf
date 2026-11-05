//! Reviewer configuration, from environment variables.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    /// Shared secret Lightleaf sends as `Authorization: Bearer …`.
    pub secret: String,
    pub anthropic_api_key: Option<String>,
    pub anthropic_base_url: String,
    pub model: String,
    pub effort: String,
    /// Send `fallbacks: "default"` (disable for gateways that do not support it).
    pub fallbacks: bool,
    /// Name of the comment macro (`\ANG` by default).
    pub macro_name: String,
    pub max_comments: usize,
    pub max_jobs: usize,
    pub agent_concurrency: usize,
    pub agent_timeout_secs: u64,
    pub max_input_chars: usize,
    pub disabled_agents: Vec<String>,
    pub prompts_dir: Option<PathBuf>,
    pub work_dir: PathBuf,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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
        let macro_name = env("LIGHTLEAF_REVIEW_MACRO").unwrap_or_else(|| "ANG".into());
        anyhow::ensure!(
            !macro_name.is_empty() && macro_name.chars().all(|c| c.is_ascii_alphabetic()),
            "LIGHTLEAF_REVIEW_MACRO must contain only letters (it becomes a LaTeX command name)"
        );
        Ok(Config {
            bind: env("LIGHTLEAF_REVIEWER_BIND").unwrap_or_else(|| "127.0.0.1:8090".into()),
            secret,
            anthropic_api_key: env("ANTHROPIC_API_KEY"),
            anthropic_base_url: env("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|| "https://api.anthropic.com".into()),
            model: env("LIGHTLEAF_REVIEW_MODEL").unwrap_or_else(|| "claude-opus-5-5".into()),
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
            work_dir: env("LIGHTLEAF_REVIEW_WORKDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("lightleaf-reviewer")),
        })
    }
}
