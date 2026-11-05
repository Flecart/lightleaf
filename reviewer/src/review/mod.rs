//! The review: classify the paper, run a set of [`Reviewer`]s, merge and rank what they found.
//!
//! The orchestrator ([`run`]) does not know what any reviewer does. It builds one shared
//! [`ReviewInput`], runs every applicable reviewer concurrently (bounded, with a timeout each),
//! anchors each [`Finding`] to a source position, deduplicates and keeps the most important.
//! A reviewer that fails is reported and skipped; it never fails the job.
//!
//! To add a reviewer, implement [`Reviewer`] and register it in [`registry::build`].

mod classify;
mod dedup;
mod llm_reviewer;
pub mod registry;
mod schema;

pub use classify::Classification;

use crate::{
    config::Config,
    latex::{self, Document},
    llm::{Llm, Usage},
    prompts::Prompts,
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinSet};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Suggestion,
    Warning,
    Critical,
}

impl Severity {
    pub fn parse(s: &str) -> Severity {
        match s {
            "critical" => Severity::Critical,
            "warning" => Severity::Warning,
            _ => Severity::Suggestion,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Warning => "warning",
            Severity::Suggestion => "suggestion",
        }
    }
}

/// What a reviewer reports: a comment about an exact passage of the source.
#[derive(Clone, Debug)]
pub struct Finding {
    pub quote: String,
    pub text: String,
    pub severity: Severity,
}

/// A finding anchored in a source file, ready to be written.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Comment {
    pub agent: String,
    pub severity: Severity,
    pub text: String,
    pub quote: String,
    pub file: String,
    /// Byte range of the quote in the file at review time.
    #[serde(skip)]
    pub range: (usize, usize),
}

/// Options chosen by the author for one review.
#[derive(Clone, Default)]
pub struct Options {
    pub venue: String,
    pub focus: String,
}

/// Everything a reviewer may look at; built once per review and shared.
pub struct ReviewInput {
    pub doc: Document,
    /// The merged source wrapped for the model (identical across calls, so it is cached).
    pub document: String,
    /// The system prompt shared by all model calls of the review.
    pub system: String,
    pub classification: Classification,
    pub options: Options,
    pub llm: Arc<Llm>,
    pub usage: Arc<Usage>,
    pub prompts: Arc<Prompts>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One specialised reviewer (an LLM agent, a rule-based check, the reference checker, ...).
pub trait Reviewer: Send + Sync {
    /// Stable identifier, used in `LIGHTLEAF_REVIEW_DISABLED_AGENTS`.
    fn id(&self) -> &str;
    /// Name shown in comment labels (`\AI[<name>, warning]{…}`).
    fn name(&self) -> &str;
    /// Most findings this reviewer may contribute.
    fn max_findings(&self) -> usize;
    /// Whether it has anything to review in this paper (e.g. no appendix, no appendix reviewer).
    fn applies(&self, _input: &ReviewInput) -> bool {
        true
    }
    fn review(
        self: Arc<Self>,
        input: Arc<ReviewInput>,
    ) -> BoxFuture<'static, anyhow::Result<Vec<Finding>>>;
}

/// Shared services the orchestrator needs.
pub struct Ctx {
    pub llm: Arc<Llm>,
    pub cfg: Arc<Config>,
    pub prompts: Arc<Prompts>,
    pub usage: Arc<Usage>,
}

pub struct Outcome {
    pub paper_type: String,
    pub summary: String,
    pub comments: Vec<Comment>,
    pub failed_agents: Vec<(String, String)>,
    pub unanchored: usize,
    pub merged_duplicates: usize,
    pub dropped_over_limit: usize,
}

/// Locates a finding's quote in the document's files.
fn anchor(doc: &Document, agent: &str, f: Finding) -> Option<Comment> {
    let text: String = f.text.trim().chars().take(1200).collect();
    if text.is_empty() {
        return None;
    }
    doc.files.iter().find_map(|file| {
        latex::find_quote(&file.text, &f.quote).map(|range| Comment {
            agent: agent.to_string(),
            severity: f.severity.clone(),
            text: text.clone(),
            quote: f.quote.clone(),
            file: file.path.clone(),
            range,
        })
    })
}

/// Runs the whole review and returns anchored comments (not yet written to the files).
pub async fn run(
    ctx: &Ctx,
    doc: Document,
    options: Options,
    progress: impl Fn(String),
) -> anyhow::Result<Outcome> {
    let merged = doc.merged();
    if merged.len() > ctx.cfg.max_input_chars {
        anyhow::bail!(
            "the document is too long to review ({} characters, limit {}); raise LIGHTLEAF_REVIEW_MAX_INPUT_CHARS",
            merged.len(),
            ctx.cfg.max_input_chars
        );
    }
    let document = format!("<paper>\n{merged}</paper>");
    let system = ctx.prompts.get("system");

    progress("Reading the paper".into());
    let classification = classify::run(ctx, &system, &document, &merged).await?;

    let input = Arc::new(ReviewInput {
        doc,
        document,
        system,
        classification,
        options,
        llm: ctx.llm.clone(),
        usage: ctx.usage.clone(),
        prompts: ctx.prompts.clone(),
    });
    let reviewers: Vec<Arc<dyn Reviewer>> = registry::build(&ctx.cfg)
        .into_iter()
        .filter(|r| !ctx.cfg.disabled_agents.iter().any(|d| d == r.id()) && r.applies(&input))
        .collect();
    progress(format!("Running {} reviewers", reviewers.len()));

    let sem = Arc::new(Semaphore::new(ctx.cfg.agent_concurrency));
    let timeout = Duration::from_secs(ctx.cfg.agent_timeout_secs);
    let mut set = JoinSet::new();
    for r in reviewers {
        let (sem, input) = (sem.clone(), input.clone());
        set.spawn(async move {
            let _permit = sem.acquire_owned().await;
            let res = match tokio::time::timeout(timeout, r.clone().review(input)).await {
                Ok(res) => res,
                Err(_) => Err(anyhow::anyhow!("timed out after {} s", timeout.as_secs())),
            };
            (r, res)
        });
    }

    let (mut raw, mut failed, mut unanchored) = (Vec::new(), Vec::new(), 0);
    while let Some(joined) = set.join_next().await {
        let Ok((r, res)) = joined else { continue };
        match res {
            Ok(findings) => {
                for f in findings.into_iter().take(r.max_findings()) {
                    match anchor(&input.doc, r.name(), f) {
                        Some(c) => raw.push(c),
                        None => unanchored += 1,
                    }
                }
                progress(format!("{} reviewer finished", r.name()));
            }
            Err(e) => {
                tracing::warn!("reviewer {} failed: {e:#}", r.id());
                failed.push((r.name().to_string(), format!("{e:#}")));
            }
        }
    }

    let (mut comments, merged_duplicates) = dedup::dedup(raw);
    comments.sort_by(|a, b| b.severity.cmp(&a.severity));
    let dropped_over_limit = comments.len().saturating_sub(ctx.cfg.max_comments);
    comments.truncate(ctx.cfg.max_comments);
    comments.sort_by(|a, b| a.file.cmp(&b.file).then(a.range.0.cmp(&b.range.0)));
    Ok(Outcome {
        paper_type: input.classification.paper_type.clone(),
        summary: input.classification.summary.clone(),
        comments,
        failed_agents: failed,
        unanchored,
        merged_duplicates,
        dropped_over_limit,
    })
}
