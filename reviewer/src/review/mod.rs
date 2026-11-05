//! The review: a [`Pack`] plans which reviewers to run, the orchestrator ([`run`]) runs them,
//! anchors what they found in the source, deduplicates and ranks.
//!
//! - A [`Pack`] is a complete review methodology (prompts, classification, which reviewers,
//!   post-processing). Packs live in [`packs`]; `LIGHTLEAF_REVIEW_PACK` picks one.
//! - A [`Reviewer`] is one specialised check: a prompt-driven model call ([`PromptReviewer`]),
//!   a rule-based check, the reference checker, ...
//!
//! The orchestrator knows neither packs' prompts nor what reviewers do. A reviewer that fails
//! is reported and skipped; it never fails the job.

mod dedup;
pub mod packs;
mod prompt_reviewer;
pub mod template;

pub use prompt_reviewer::PromptReviewer;

use crate::{
    config::Config,
    latex::{self, Document},
    llm::{Llm, Usage},
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
        match s.trim().to_ascii_lowercase().as_str() {
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
    /// From a reviewer that reads the whole paper; loses ties against section reviewers.
    #[serde(skip)]
    pub broad: bool,
}

/// Options chosen by the author for one review.
#[derive(Clone, Default, Debug)]
pub struct Options {
    /// Target venue id (pack-specific, e.g. `neurips_2025`), or free text, or empty.
    pub venue: String,
    pub focus: String,
    /// Only report issues that would matter to a reviewer; fewer comments.
    pub strict: bool,
}

/// Model access shared by all reviewers of a job.
pub struct Services {
    pub llm: Arc<Llm>,
    pub usage: Arc<Usage>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One specialised reviewer.
pub trait Reviewer: Send + Sync {
    /// Stable identifier, used in `LIGHTLEAF_REVIEW_DISABLED_AGENTS`.
    fn id(&self) -> &str;
    /// Name shown in comment labels (`\AI[<name>, warning]{…}`).
    fn name(&self) -> &str;
    /// Most findings this reviewer may contribute.
    fn max_findings(&self) -> usize;
    /// Reads the whole paper (loses dedup ties against section-specific reviewers).
    fn broad(&self) -> bool {
        false
    }
    fn review(
        self: Arc<Self>,
        services: Arc<Services>,
    ) -> BoxFuture<'static, anyhow::Result<Vec<Finding>>>;
}

/// What a pack decided after reading the paper.
pub struct Plan {
    pub paper_type: String,
    pub summary: String,
    pub reviewers: Vec<Arc<dyn Reviewer>>,
}

/// A review methodology.
pub trait Pack: Send + Sync {
    fn id(&self) -> &'static str;
    /// Venues this pack has specific guidance for, as (id, label).
    fn venues(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Reads the paper (typically one classification call) and builds the reviewers.
    fn plan<'a>(
        &'a self,
        ctx: &'a Ctx,
        doc: &'a Document,
        options: &'a Options,
    ) -> BoxFuture<'a, anyhow::Result<Plan>>;
    /// Post-processing after deduplication (e.g. pruning in strict mode). Default: unchanged.
    fn finalize<'a>(
        &'a self,
        _ctx: &'a Ctx,
        comments: Vec<Comment>,
        _options: &'a Options,
    ) -> BoxFuture<'a, Vec<Comment>> {
        Box::pin(async move { comments })
    }
}

/// Shared services the orchestrator and packs need.
pub struct Ctx {
    pub llm: Arc<Llm>,
    pub cfg: Arc<Config>,
    pub pack: Arc<dyn Pack>,
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

/// Locates a finding's quote in the document's files (exactly, then fuzzily).
fn anchor(doc: &Document, r: &dyn Reviewer, f: Finding) -> Option<Comment> {
    let text: String = f.text.trim().chars().take(1200).collect();
    if text.is_empty() {
        return None;
    }
    let exact = doc
        .files
        .iter()
        .find_map(|file| latex::find_quote(&file.text, &f.quote).map(|range| (file, range)));
    let (file, range) = exact.or_else(|| {
        doc.files.iter().find_map(|file| {
            latex::anchor::find_fuzzy(&file.text, &f.quote).map(|range| (file, range))
        })
    })?;
    Some(Comment {
        agent: r.name().to_string(),
        severity: f.severity,
        text,
        // The text actually found, so later re-anchoring finds it again.
        quote: file.text[range.0..range.1].to_string(),
        file: file.path.clone(),
        range,
        broad: r.broad(),
    })
}

/// Runs the whole review and returns anchored comments (not yet written to the files).
pub async fn run(
    ctx: &Ctx,
    doc: Document,
    options: Options,
    progress: impl Fn(String),
) -> anyhow::Result<Outcome> {
    let total = doc.total_len();
    if total > ctx.cfg.max_input_chars {
        anyhow::bail!(
            "the document is too long to review ({total} characters, limit {}); raise LIGHTLEAF_REVIEW_MAX_INPUT_CHARS",
            ctx.cfg.max_input_chars
        );
    }
    progress("Reading the paper".into());
    let plan = ctx.pack.plan(ctx, &doc, &options).await?;
    let reviewers: Vec<Arc<dyn Reviewer>> = plan
        .reviewers
        .into_iter()
        .filter(|r| !ctx.cfg.disabled_agents.iter().any(|d| d == r.id()))
        .collect();
    progress(format!("Running {} reviewers", reviewers.len()));

    let services = Arc::new(Services {
        llm: ctx.llm.clone(),
        usage: ctx.usage.clone(),
    });
    let sem = Arc::new(Semaphore::new(ctx.cfg.agent_concurrency));
    let timeout = Duration::from_secs(ctx.cfg.agent_timeout_secs);
    let mut set = JoinSet::new();
    for r in reviewers {
        let (sem, services) = (sem.clone(), services.clone());
        set.spawn(async move {
            let _permit = sem.acquire_owned().await;
            let res = match tokio::time::timeout(timeout, r.clone().review(services)).await {
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
                    if options.strict && f.severity == Severity::Suggestion {
                        continue;
                    }
                    match anchor(&doc, r.as_ref(), f) {
                        Some(c) => raw.push(c),
                        None => unanchored += 1,
                    }
                }
                progress(format!("{} finished", r.name()));
            }
            Err(e) => {
                tracing::warn!("reviewer {} failed: {e:#}", r.id());
                failed.push((r.name().to_string(), format!("{e:#}")));
            }
        }
    }

    let (comments, merged_duplicates) = dedup::dedup(raw);
    let mut comments = ctx.pack.finalize(ctx, comments, &options).await;
    comments.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.broad.cmp(&b.broad)));
    let dropped_over_limit = comments.len().saturating_sub(ctx.cfg.max_comments);
    comments.truncate(ctx.cfg.max_comments);
    comments.sort_by(|a, b| a.file.cmp(&b.file).then(a.range.0.cmp(&b.range.0)));
    Ok(Outcome {
        paper_type: plan.paper_type,
        summary: plan.summary,
        comments,
        failed_agents: failed,
        unanchored,
        merged_duplicates,
        dropped_over_limit,
    })
}
