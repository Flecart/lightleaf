//! Jobs Lightleaf asks for. Each kind is one module with a `run` function:
//!
//! - [`review`]: run the reviewers and write their comments into the source,
//! - [`references`]: check the bibliography against Semantic Scholar,
//! - [`clear`]: remove every review comment.
//!
//! All of them change the project the same way, through [`publish::publish`]: edit a fresh
//! checkout, commit, push, and redo the edit on top of the latest version if the push raced
//! with someone else's changes.

mod clear;
mod publish;
mod references;
mod review;
pub mod store;

use crate::{git::Repo, review::Ctx};
use serde::Deserialize;
use serde_json::Value;
use std::{sync::Arc, time::SystemTime};

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub name: String,
    pub email: String,
}

/// A job as sent by Lightleaf (`POST /v1/jobs`).
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

/// The job kinds this service understands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Review,
    References,
    Clear,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "review" => Some(Kind::Review),
            "references" => Some(Kind::References),
            "clear" => Some(Kind::Clear),
            _ => None,
        }
    }

    /// Whether this kind of job calls the model (and so needs credentials).
    pub fn needs_llm(self) -> bool {
        matches!(self, Kind::Review)
    }
}

impl JobRequest {
    pub fn validate(&self) -> Result<Kind, String> {
        let kind = Kind::parse(&self.kind)
            .ok_or("kind must be \"review\", \"references\" or \"clear\"")?;
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
        Ok(kind)
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Today's date as YYYY-MM-DD (UTC).
pub fn today() -> String {
    let days = (now() / 86_400) as i64;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Progress reporting from a running job.
pub type Progress = dyn Fn(String) + Send + Sync;

/// Runs a job to completion and returns its result (shown in the Review dialog).
pub async fn run(
    ctx: Arc<Ctx>,
    kind: Kind,
    req: JobRequest,
    job_id: String,
    progress: Box<Progress>,
) -> anyhow::Result<Value> {
    let work = ctx.cfg.work_dir.join(&job_id);
    let result = async {
        progress("Fetching the project".into());
        let repo = Repo::clone(&req.git_url, &req.token, work.join("repo")).await?;
        match kind {
            Kind::Review => review::run(&ctx, &req, &repo, progress.as_ref()).await,
            Kind::References => references::run(&ctx, &req, &repo, progress.as_ref()).await,
            Kind::Clear => clear::run(&ctx, &req, &repo, progress.as_ref()).await,
        }
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    result
}
