//! Checking the bibliography: verify cited references, then write a comment after the first
//! `\cite` of each problematic one.

use super::{
    JobRequest, Progress,
    publish::{Edit, publish},
};
use crate::{
    git::Repo,
    latex::{
        self,
        annotate::{self, Annotation},
    },
    references::{self, s2},
    review::Ctx,
};
use serde_json::{Value, json};

pub async fn run(
    ctx: &Ctx,
    req: &JobRequest,
    repo: &Repo,
    progress: &Progress,
) -> anyhow::Result<Value> {
    let doc = latex::gather(&repo.dir, &req.main_file)?;
    let client = s2::Client::new(ctx.cfg.s2.clone())?;
    let report = references::check(
        &repo.dir,
        &doc,
        &client,
        ctx.cfg.references_max_entries,
        progress,
    )
    .await?;
    let notes: Vec<Annotation> = report
        .problems
        .iter()
        .filter_map(|p| {
            let (file, command) = p.cited_at.as_ref()?;
            Some(Annotation {
                file,
                quote: command,
                label: format!("References, {}", p.severity.as_str()),
                text: &p.message,
            })
        })
        .collect();
    let (applied, commit) = if notes.is_empty() {
        (Default::default(), None)
    } else {
        progress(format!("Writing {} comments", notes.len()));
        publish(repo, progress, |dir| {
            let applied = annotate::apply(dir, &req.main_file, &notes, &ctx.cfg.macro_name)?;
            let message = (applied.written > 0).then(|| {
                format!(
                    "Reference check: {} comment{}\n\nRequested by {} <{}>. Checked {} cited references against Semantic Scholar.\n",
                    applied.written,
                    if applied.written == 1 { "" } else { "s" },
                    req.requested_by.name,
                    req.requested_by.email,
                    report.checked
                )
            });
            Ok(Edit { message, outcome: applied })
        })
        .await?
    };
    Ok(json!({
        "references": report,
        "written": applied.written,
        "skipped": applied.skipped,
        "commit": commit,
    }))
}
