//! Review and clear jobs: clone the project, do the work, write comments into the source,
//! commit and push. Pushes that race with concurrent edits are retried on top of the latest
//! version (comments are re-anchored by their quotes, so they land in the right place).

use crate::{
    git::Repo,
    latex,
    pipeline::{self, Comment, Ctx, Options},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::SystemTime};

const PUSH_ATTEMPTS: usize = 4;

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub name: String,
    pub email: String,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JobRequest {
    pub kind: String,
    pub project_id: String,
    #[serde(default)]
    pub project_name: String,
    #[serde(default)]
    pub compiler: String,
    pub main_file: String,
    pub git_url: String,
    pub token: String,
    pub requested_by: Person,
    #[serde(default)]
    pub options: Value,
}

impl JobRequest {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.kind.as_str(), "review" | "clear") {
            return Err("kind must be \"review\" or \"clear\"".into());
        }
        if self.project_id.is_empty() || !self.project_id.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err("invalid projectId".into());
        }
        if !(self.git_url.starts_with("http://") || self.git_url.starts_with("https://")) {
            return Err("gitUrl must be an http(s) URL".into());
        }
        if self.token.is_empty() {
            return Err("missing token".into());
        }
        Ok(())
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    /// queued | running | done | failed
    pub status: String,
    pub stage: String,
    pub created_at: u64,
    pub finished_at: Option<u64>,
    pub error: Option<String>,
    pub result: Option<Value>,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Writes `comments` into the checkout. Returns (comments written, comments skipped).
fn write_comments(
    dir: &Path,
    main_file: &str,
    comments: &[Comment],
    macro_name: &str,
) -> anyhow::Result<(usize, usize)> {
    let mut by_file: BTreeMap<&str, Vec<&Comment>> = BTreeMap::new();
    for c in comments {
        by_file.entry(c.file.as_str()).or_default().push(c);
    }
    let (mut written, mut skipped) = (0, 0);
    for (file, list) in by_file {
        let path = dir.join(file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            skipped += list.len();
            continue;
        };
        // Points in the *current* text (the file may have changed since the review ran).
        let mut inserts: Vec<(usize, String)> = Vec::new();
        for c in list {
            let point = latex::find_quote(&text, &c.quote)
                .and_then(|(_, end)| latex::insertion_point(&text, end));
            match point {
                Some(p) => {
                    let label = format!("{}, {}", c.agent, c.severity.as_str());
                    inserts.push((p, latex::comment_macro(macro_name, &label, &c.text)));
                    written += 1;
                }
                None => skipped += 1,
            }
        }
        if inserts.is_empty() {
            continue;
        }
        // Insert from the end so earlier offsets stay valid; keep the order of comments that
        // share a point.
        inserts.sort_by_key(|(p, _)| *p);
        let mut out = text.clone();
        for (p, m) in inserts.into_iter().rev() {
            out.insert_str(p, &m);
        }
        std::fs::write(&path, out)?;
    }
    if written > 0 {
        let sty = dir.join(format!("{}.sty", latex::PACKAGE));
        if !sty.exists() {
            std::fs::write(&sty, latex::package_source(macro_name))?;
        }
        let main_path = dir.join(main_file);
        let main = std::fs::read_to_string(&main_path)?;
        if let Some(updated) = latex::ensure_package(&main)? {
            std::fs::write(&main_path, updated)?;
        }
    }
    Ok((written, skipped))
}

/// Removes AI comments from every `.tex` file. Returns how many were removed.
fn clear_comments(dir: &Path, macro_name: &str) -> anyhow::Result<usize> {
    let mut removed = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)?.flatten() {
            let p = e.path();
            let ft = e.file_type()?;
            if ft.is_symlink() || e.file_name() == ".git" {
                continue;
            }
            if ft.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "tex")
                && let Ok(text) = std::fs::read_to_string(&p)
            {
                let (clean, n) = latex::remove_ai_comments(&text, macro_name);
                if n > 0 {
                    std::fs::write(&p, clean)?;
                    removed += n;
                }
            }
        }
    }
    Ok(removed)
}

/// Runs a job to completion. `update` receives stage messages.
pub async fn run(
    ctx: Arc<Ctx>,
    req: JobRequest,
    job_id: String,
    update: impl Fn(String) + Send + Sync,
) -> anyhow::Result<Value> {
    let dir = ctx.cfg.work_dir.join(&job_id).join("repo");
    let result = run_in(&ctx, &req, &dir, &update).await;
    let _ = tokio::fs::remove_dir_all(dir.parent().unwrap_or(&dir)).await;
    result
}

async fn run_in(
    ctx: &Ctx,
    req: &JobRequest,
    dir: &Path,
    update: &(impl Fn(String) + Send + Sync),
) -> anyhow::Result<Value> {
    let macro_name = ctx.cfg.macro_name.clone();
    if req.kind == "review" && req.compiler == "typst" {
        anyhow::bail!("AI review currently supports LaTeX projects only");
    }
    update("Fetching the project".into());
    let repo = Repo::clone(&req.git_url, &req.token, dir.to_path_buf()).await?;

    if req.kind == "clear" {
        for attempt in 1..=PUSH_ATTEMPTS {
            if attempt > 1 {
                repo.sync().await?;
            }
            let n = clear_comments(&repo.dir, &macro_name)?;
            if n == 0 {
                return Ok(json!({ "removed": 0, "commit": null }));
            }
            let msg = format!(
                "Remove {n} AI review comment{}\n\nRequested by {} <{}>.\n",
                if n == 1 { "" } else { "s" },
                req.requested_by.name,
                req.requested_by.email
            );
            let sha = repo.commit(&msg).await?;
            if repo.push().await? {
                return Ok(json!({ "removed": n, "commit": sha }));
            }
            update("Project changed meanwhile, retrying".into());
        }
        anyhow::bail!("could not push: the project kept changing; try again");
    }

    let doc = latex::gather(&repo.dir, &req.main_file)?;
    if doc.total_len() == 0 {
        anyhow::bail!("the main file is empty");
    }
    let opts = Options {
        venue: req.options["venue"].as_str().unwrap_or("").to_string(),
        focus: req.options["focus"].as_str().unwrap_or("").to_string(),
    };
    let outcome = pipeline::review(ctx, &doc, &opts, update).await?;
    if outcome.comments.is_empty() {
        return Ok(summary(ctx, &outcome, 0, 0, None));
    }

    update(format!("Writing {} comments", outcome.comments.len()));
    for attempt in 1..=PUSH_ATTEMPTS {
        if attempt > 1 {
            repo.sync().await?;
        }
        let (written, skipped) =
            write_comments(&repo.dir, &req.main_file, &outcome.comments, &macro_name)?;
        if written == 0 {
            return Ok(summary(ctx, &outcome, 0, skipped, None));
        }
        let agents: Vec<&str> = {
            let mut a: Vec<&str> = outcome.comments.iter().map(|c| c.agent.as_str()).collect();
            a.sort();
            a.dedup();
            a
        };
        let msg = format!(
            "AI review: {written} comment{}\n\nRequested by {} <{}>.\nModel: {}. Reviewers: {}.\n\
             Comments are \\{macro_name}[AI: ...]{{...}} macros; remove them all with \"Remove AI comments\"\n\
             in the Review dialog, hide them with \\usepackage[hide]{{{}}}, or revert this commit.\n",
            if written == 1 { "" } else { "s" },
            req.requested_by.name,
            req.requested_by.email,
            ctx.llm.model(),
            agents.join(", "),
            latex::PACKAGE,
        );
        let sha = repo.commit(&msg).await?;
        if repo.push().await? {
            return Ok(summary(ctx, &outcome, written, skipped, sha));
        }
        update("Project changed meanwhile, re-applying comments".into());
    }
    anyhow::bail!("could not push: the project kept changing; try again")
}

fn summary(
    ctx: &Ctx,
    o: &pipeline::Outcome,
    written: usize,
    skipped: usize,
    commit: Option<String>,
) -> Value {
    let mut by_severity: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_agent: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &o.comments {
        *by_severity.entry(c.severity.as_str()).or_default() += 1;
        *by_agent.entry(c.agent.as_str()).or_default() += 1;
    }
    json!({
        "paperType": o.paper_type,
        "summary": o.summary,
        "written": written,
        "skipped": skipped,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::Severity;

    #[test]
    fn writes_and_clears_comments_in_a_checkout() {
        let dir = std::env::temp_dir().join(format!("llr-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("sec")).unwrap();
        std::fs::write(dir.join("main.tex"), "\\documentclass{article}\n\\begin{document}\nWe show that our method is always better.\n\\input{sec/a}\n\\end{document}\n").unwrap();
        std::fs::write(
            dir.join("sec/a.tex"),
            "Results are in Table 1 and look good.\n",
        )
        .unwrap();
        let comments = vec![
            Comment {
                agent: "Results".into(),
                severity: Severity::Critical,
                text: "\"Always\" is not supported by the experiments.".into(),
                quote: "our method is always better".into(),
                file: "main.tex".into(),
                range: (0, 0),
            },
            Comment {
                agent: "Writing".into(),
                severity: Severity::Suggestion,
                text: "Say what the table shows.".into(),
                quote: "Results are in Table 1".into(),
                file: "sec/a.tex".into(),
                range: (0, 0),
            },
            Comment {
                agent: "Writing".into(),
                severity: Severity::Suggestion,
                text: "x".into(),
                quote: "not present anywhere in the files".into(),
                file: "main.tex".into(),
                range: (0, 0),
            },
        ];
        let (w, s) = write_comments(&dir, "main.tex", &comments, "ANG").unwrap();
        assert_eq!((w, s), (2, 1));
        let main = std::fs::read_to_string(dir.join("main.tex")).unwrap();
        assert!(main.contains("\\usepackage{lightleaf-comments}"));
        assert!(main.contains("always better. \\ANG[AI: Results, critical]{\"Always\" is not supported by the experiments.}\n"));
        assert!(dir.join("lightleaf-comments.sty").exists());
        assert_eq!(clear_comments(&dir, "ANG").unwrap(), 2);
        let a = std::fs::read_to_string(dir.join("sec/a.tex")).unwrap();
        assert_eq!(a, "Results are in Table 1 and look good.\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
