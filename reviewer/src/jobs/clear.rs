//! Removing every review comment from the project.

use super::{
    JobRequest, Progress,
    publish::{Edit, publish},
};
use crate::{git::Repo, latex::annotate, review::Ctx};
use serde_json::{Value, json};

pub async fn run(
    ctx: &Ctx,
    req: &JobRequest,
    repo: &Repo,
    progress: &Progress,
) -> anyhow::Result<Value> {
    let macro_name = &ctx.cfg.macro_name;
    progress("Removing comments".into());
    let (removed, commit) = publish(repo, progress, |dir| {
        let n = annotate::clear(dir, macro_name)?;
        let message = (n > 0).then(|| {
            format!(
                "Remove {n} AI review comment{}\n\nRequested by {} <{}>.\n",
                if n == 1 { "" } else { "s" },
                req.requested_by.name,
                req.requested_by.email
            )
        });
        Ok(Edit {
            message,
            outcome: n,
        })
    })
    .await?;
    Ok(json!({ "removed": removed, "commit": commit }))
}
