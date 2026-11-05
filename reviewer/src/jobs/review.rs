//! A full review: run the reviewers, then write their comments into the source.

use super::{
    JobRequest, Progress,
    publish::{Edit, publish},
};
use crate::{
    git::Repo,
    latex::{
        self, PACKAGE,
        annotate::{self, Annotation, Applied},
    },
    review::{self, Ctx, Options, Outcome},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub async fn run(
    ctx: &Ctx,
    req: &JobRequest,
    repo: &Repo,
    progress: &Progress,
) -> anyhow::Result<Value> {
    if req.compiler == "typst" {
        anyhow::bail!("AI review currently supports LaTeX projects only");
    }
    let doc = latex::gather(&repo.dir, &req.main_file)?;
    if doc.total_len() == 0 {
        anyhow::bail!("the main file is empty");
    }
    let options = Options {
        venue: req.options["venue"].as_str().unwrap_or("").to_string(),
        focus: req.options["focus"].as_str().unwrap_or("").to_string(),
    };
    let outcome = review::run(ctx, doc, options, progress).await?;
    if outcome.comments.is_empty() {
        return Ok(summary(ctx, &outcome, &Applied::default(), None));
    }

    progress(format!("Writing {} comments", outcome.comments.len()));
    let notes: Vec<Annotation> = outcome
        .comments
        .iter()
        .map(|c| Annotation {
            file: &c.file,
            quote: &c.quote,
            label: format!("{}, {}", c.agent, c.severity.as_str()),
            text: &c.text,
        })
        .collect();
    let (applied, commit) = publish(repo, progress, |dir| {
        let applied = annotate::apply(dir, &req.main_file, &notes, &ctx.cfg.macro_name)?;
        let message =
            (applied.written > 0).then(|| commit_message(ctx, req, &outcome, applied.written));
        Ok(Edit {
            message,
            outcome: applied,
        })
    })
    .await?;
    Ok(summary(ctx, &outcome, &applied, commit))
}

fn commit_message(ctx: &Ctx, req: &JobRequest, o: &Outcome, written: usize) -> String {
    let mut agents: Vec<&str> = o.comments.iter().map(|c| c.agent.as_str()).collect();
    agents.sort();
    agents.dedup();
    let m = &ctx.cfg.macro_name;
    format!(
        "AI review: {written} comment{}\n\nRequested by {} <{}>.\nModel: {} ({}). Reviewers: {}.\n\
         Comments are \\{m}[...]{{...}} macros. Remove them all with \"Remove AI comments\" in the\n\
         Review dialog, hide them with \\usepackage[hide]{{{PACKAGE}}}, or revert this commit.\n",
        if written == 1 { "" } else { "s" },
        req.requested_by.name,
        req.requested_by.email,
        ctx.llm.model(),
        ctx.llm.provider(),
        agents.join(", "),
    )
}

fn summary(ctx: &Ctx, o: &Outcome, applied: &Applied, commit: Option<String>) -> Value {
    let mut by_severity: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_agent: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &o.comments {
        *by_severity.entry(c.severity.as_str()).or_default() += 1;
        *by_agent.entry(c.agent.as_str()).or_default() += 1;
    }
    json!({
        "paperType": o.paper_type,
        "summary": o.summary,
        "written": applied.written,
        "skipped": applied.skipped,
        "unanchored": o.unanchored,
        "mergedDuplicates": o.merged_duplicates,
        "droppedOverLimit": o.dropped_over_limit,
        "bySeverity": by_severity,
        "byAgent": by_agent,
        "failedAgents": o.failed_agents.iter().map(|(a, e)| json!({ "agent": a, "error": e })).collect::<Vec<_>>(),
        "comments": o.comments,
        "commit": commit,
        "model": ctx.llm.model(),
        "provider": ctx.llm.provider(),
        "usage": ctx.usage.to_json(),
    })
}
