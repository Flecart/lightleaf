//! The one way jobs change a project: edit a fresh checkout, commit, push. If the push is
//! rejected because the project changed meanwhile, fetch the latest version and edit again
//! (edits are expressed by content, e.g. comments anchored by quotes, so they re-apply cleanly).

use super::Progress;
use crate::git::Repo;
use std::path::Path;

const ATTEMPTS: usize = 4;

/// The result of editing a checkout once.
pub struct Edit<T> {
    /// Commit message; `None` = nothing changed, so nothing to commit.
    pub message: Option<String>,
    pub outcome: T,
}

/// Applies `edit` and pushes it. Returns the last edit's outcome and the pushed commit.
pub async fn publish<T>(
    repo: &Repo,
    progress: &Progress,
    mut edit: impl FnMut(&Path) -> anyhow::Result<Edit<T>>,
) -> anyhow::Result<(T, Option<String>)> {
    for attempt in 1..=ATTEMPTS {
        if attempt > 1 {
            progress("The project changed meanwhile; applying again".into());
            repo.sync().await?;
        }
        let Edit { message, outcome } = edit(&repo.dir)?;
        let Some(message) = message else {
            return Ok((outcome, None));
        };
        let commit = repo.commit(&message).await?;
        if repo.push().await? {
            return Ok((outcome, commit));
        }
    }
    anyhow::bail!("could not push: the project kept changing; try again")
}
