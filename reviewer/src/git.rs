//! Git access to a Lightleaf project: clone with a project-scoped token, commit as the
//! reviewer, and push (rebasing onto edits made in the meantime by re-applying the change).

use anyhow::{Context, bail};
use base64::Engine;
use std::path::{Path, PathBuf};
use tokio::process::Command;

const AUTHOR_NAME: &str = "Lightleaf Reviewer";
const AUTHOR_EMAIL: &str = "reviewer@lightleaf.invalid";

pub struct Repo {
    pub dir: PathBuf,
    auth_header: String,
}

impl Repo {
    fn cmd(&self, cwd: &Path) -> Command {
        let mut c = Command::new("git");
        c.current_dir(cwd)
            // The token travels in a header, never in the URL or .git/config.
            .arg("-c")
            .arg(format!("http.extraHeader={}", self.auth_header))
            .arg("-c")
            .arg("core.hooksPath=/dev/null")
            .arg("-c")
            .arg("credential.helper=")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .kill_on_drop(true);
        c
    }

    async fn run(&self, cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
        let out = self
            .cmd(cwd)
            .args(args)
            .output()
            .await
            .context("running git")?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Shallow-clones `url` into `dir` (which must not exist).
    pub async fn clone(url: &str, token: &str, dir: PathBuf) -> anyhow::Result<Repo> {
        let basic = base64::engine::general_purpose::STANDARD.encode(format!("reviewer:{token}"));
        let repo = Repo {
            dir,
            auth_header: format!("Authorization: Basic {basic}"),
        };
        let parent = repo
            .dir
            .parent()
            .context("work dir has no parent")?
            .to_path_buf();
        tokio::fs::create_dir_all(&parent).await?;
        let dest = repo.dir.to_string_lossy().to_string();
        repo.run(&parent, &["clone", "-q", "--depth", "1", url, &dest])
            .await?;
        Ok(repo)
    }

    /// Resets the checkout to the server's latest commit.
    pub async fn sync(&self) -> anyhow::Result<()> {
        self.run(
            &self.dir,
            &["fetch", "-q", "--depth", "1", "origin", "HEAD"],
        )
        .await?;
        self.run(&self.dir, &["reset", "-q", "--hard", "FETCH_HEAD"])
            .await?;
        Ok(())
    }

    /// Commits all changes. Returns `None` when there was nothing to commit.
    pub async fn commit(&self, message: &str) -> anyhow::Result<Option<String>> {
        self.run(&self.dir, &["add", "-A"]).await?;
        if self
            .run(&self.dir, &["status", "--porcelain"])
            .await?
            .is_empty()
        {
            return Ok(None);
        }
        self.run(
            &self.dir,
            &[
                "-c",
                &format!("user.name={AUTHOR_NAME}"),
                "-c",
                &format!("user.email={AUTHOR_EMAIL}"),
                "commit",
                "-q",
                "-m",
                message,
            ],
        )
        .await?;
        Ok(Some(self.run(&self.dir, &["rev-parse", "HEAD"]).await?))
    }

    /// Pushes HEAD to the server's current branch. `Ok(false)` means the push was rejected
    /// because someone pushed or edited in the meantime (fetch, re-apply, try again).
    pub async fn push(&self) -> anyhow::Result<bool> {
        let out = self
            .cmd(&self.dir)
            .args(["push", "-q", "origin", "HEAD:refs/heads/main"])
            .output()
            .await?;
        if out.status.success() {
            return Ok(true);
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("rejected")
            || err.contains("non-fast-forward")
            || err.contains("fetch first")
        {
            return Ok(false);
        }
        bail!("git push failed: {}", err.trim())
    }
}
