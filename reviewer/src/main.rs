//! lightleaf-reviewer: the AI review service for Lightleaf.
//!
//! A separate process that Lightleaf calls over HTTP ([`api`]). It reads a project over git with
//! a short-lived, project-scoped token ([`git`]), runs a set of reviewers on Claude or an
//! OpenAI-compatible model ([`review`], [`llm`]), and pushes the comments back as
//! `\AI[…]{…}` macros in the LaTeX source ([`latex`], [`jobs`]). It has no database: jobs live
//! in memory and checkouts in a temporary directory.

mod api;
mod config;
mod git;
mod jobs;
mod latex;
mod llm;
mod references;
mod review;

use std::sync::Arc;
use tokio::sync::Semaphore;

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lightleaf_reviewer=info"));
    if std::env::var("LIGHTLEAF_LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json")) {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

/// The model provider selected by the configuration.
fn provider(cfg: &config::Config) -> Box<dyn llm::Provider> {
    match cfg.provider.as_str() {
        "openai" => Box::new(llm::OpenAi {
            base_url: cfg.openai_base_url.clone(),
            api_key: cfg.openai_api_key.clone().unwrap_or_default(),
            model: cfg.model.clone(),
            reasoning_effort: cfg.openai_reasoning_effort.clone(),
            max_tokens_field: cfg.openai_max_tokens_field.clone(),
        }),
        _ => Box::new(llm::Anthropic {
            base_url: cfg.anthropic_base_url.clone(),
            api_key: cfg.anthropic_api_key.clone().unwrap_or_default(),
            model: cfg.model.clone(),
            effort: cfg.effort.clone(),
            fallbacks: cfg.fallbacks,
        }),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("lightleaf-reviewer {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    init_logging();
    let cfg = Arc::new(config::Config::from_env()?);
    if !cfg.llm_configured() {
        tracing::warn!(
            "{}: reviews will be refused (removing comments still works)",
            cfg.llm_missing_message()
        );
    }
    std::fs::create_dir_all(&cfg.work_dir)?;
    let app = Arc::new(api::App {
        llm: Arc::new(llm::Llm::new(provider(&cfg))?),
        pack: review::packs::with_settings(&cfg)?,
        jobs: Default::default(),
        slots: Arc::new(Semaphore::new(cfg.max_jobs)),
        cfg: cfg.clone(),
    });
    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(
        "lightleaf-reviewer listening on http://{} ({} model {})",
        cfg.bind,
        cfg.provider,
        cfg.model
    );
    axum::serve(listener, api::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
