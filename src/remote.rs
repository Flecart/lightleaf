//! Sync with an external git remote: GitHub, GitLab, Gitea, or Overleaf's git bridge
//! (`https://git.overleaf.com/<project id>`, username `git`, password = an Overleaf git token).
//!
//! Credentials are kept out of the project directory, because compiled documents can read files
//! there (Typst reads anything under the project root):
//! * the remote URL in `.git/config` never contains userinfo; a URL pasted with
//!   `user:token@` is split and the token moved to the database;
//! * the token is stored sealed (`secrets.rs`, bound to the project id) in `remote_credentials`,
//!   is never returned by the API, and is dropped when the remote's host changes;
//! * git receives it through an environment-only credential helper that answers only for the
//!   remote's host over https. Every other helper is disabled for these runs, so the server
//!   user's own helpers (e.g. `store`) cannot cache it;
//! * git output is redacted before it reaches an error message or a log.
//!
//! Only the owner can set or change the remote and its token. Editors can pull, push and sync,
//! which uses the owner's token for this one remote.

use crate::{
    auth::User,
    error::{AppError, AppResult},
    git::{commit_pending, git, git_cmd},
    state::{Role, SharedState},
    util::now,
};
use axum::{
    Json,
    extract::{Path, State},
};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path as FsPath, time::Duration};

const TIMEOUT_SECS: u64 = 120;
const DEFAULT_USERNAME: &str = "git";

/// Answers `get` with the credential from the environment, and only for the expected host over
/// https. `store`/`erase` are ignored. Secrets travel in the environment of the git process,
/// readable only by the server user, never on a command line.
const HELPER: &str = r#"!f() { test "$1" = get || exit 0; p=; h=; while IFS= read -r l && [ -n "$l" ]; do case "$l" in protocol=*) p=${l#protocol=};; host=*) h=${l#host=};; esac; done; [ "$p" = https ] && [ "$h" = "$LIGHTLEAF_CRED_HOST" ] || exit 0; printf 'username=%s\npassword=%s\n' "$LIGHTLEAF_CRED_USER" "$LIGHTLEAF_CRED_TOKEN"; }; f"#;

pub struct Credential {
    pub host: String,
    pub username: String,
    pub token: String,
}

/// `scheme://[userinfo@]host[:port]/path` -> (URL without userinfo, userinfo).
fn split_userinfo(url: &str) -> (String, Option<String>) {
    let Some(s) = url.find("://") else {
        return (url.to_string(), None);
    };
    let rest = &url[s + 3..];
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..auth_end].rfind('@') {
        Some(at) => (
            format!("{}{}", &url[..s + 3], &rest[at + 1..]),
            Some(rest[..at].to_string()),
        ),
        None => (url.to_string(), None),
    }
}

/// `host[:port]`, lowercased, of an http(s) URL without userinfo.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    (!host.is_empty() && !host.contains('@')).then(|| host.to_ascii_lowercase())
}

/// Userinfo pasted in a URL -> (username, token). `token@host` (GitHub style) uses a placeholder
/// username, which GitHub, GitLab and Gitea accept with a personal access token.
fn userinfo_credential(info: &str) -> Option<(String, String)> {
    let (u, t) = info.split_once(':').unwrap_or((info, ""));
    if !t.is_empty() {
        Some((percent_decode(u), percent_decode(t)))
    } else if !u.is_empty() {
        Some(("x-access-token".into(), percent_decode(u)))
    } else {
        None
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        if b[i] == b'%'
            && let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Removes the token, and any userinfo in URLs, from git output.
fn redact(msg: &str, cred: Option<&Credential>) -> String {
    let mut m = msg.to_string();
    if let Some(c) = cred
        && c.token.len() >= 4
    {
        m = m.replace(&c.token, "***");
    }
    m.split(' ')
        .map(|w| match w.find("://") {
            Some(_) => split_userinfo(w).0,
            None => w.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn load_credential(state: &SharedState, id: &str) -> AppResult<Option<Credential>> {
    let pid = id.to_string();
    let row = state
        .db(move |c| {
            c.query_row(
                "SELECT host, username, secret FROM remote_credentials WHERE project_id = ?1",
                [&pid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()
        })
        .await?;
    let Some((host, username, blob)) = row else {
        return Ok(None);
    };
    let token = secrets(state)?
        .open(id, &blob)
        .map_err(|e| AppError::conflict(format!("{e}; set the remote token again")))?;
    Ok(Some(Credential {
        host,
        username,
        token,
    }))
}

async fn store_credential(
    state: &SharedState,
    id: &str,
    user: i64,
    c: &Credential,
) -> AppResult<()> {
    let blob = secrets(state)?.seal(id, &c.token);
    let (pid, host, username) = (id.to_string(), c.host.clone(), c.username.clone());
    state
        .db(move |db| {
            db.execute(
                "INSERT INTO remote_credentials (project_id, host, username, secret, set_by, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(project_id) DO UPDATE SET host = ?2, username = ?3, secret = ?4,
                   set_by = ?5, updated_at = ?6",
                rusqlite::params![pid, host, username, blob, user, now()],
            )
        })
        .await?;
    Ok(())
}

async fn delete_credential(state: &SharedState, id: &str) -> AppResult<()> {
    let pid = id.to_string();
    state
        .db(move |c| {
            c.execute(
                "DELETE FROM remote_credentials WHERE project_id = ?1",
                [&pid],
            )
        })
        .await?;
    Ok(())
}

fn secrets(state: &SharedState) -> AppResult<&crate::secrets::SecretBox> {
    state
        .secrets
        .get()
        .ok_or_else(|| AppError::conflict("credential storage is not available"))
}

async fn remote_url(dir: &FsPath) -> Option<String> {
    let url = git(dir, &["config", "--get", "remote.origin.url"])
        .await
        .ok()?;
    let url = url.trim();
    (!url.is_empty()).then(|| url.to_string())
}

/// Moves a token left in `.git/config` by older versions (or a manual edit) into the database.
async fn migrate_legacy_url(
    state: &SharedState,
    id: &str,
    dir: &FsPath,
    owner: i64,
) -> AppResult<()> {
    let Some(url) = remote_url(dir).await else {
        return Ok(());
    };
    let (clean, info) = split_userinfo(&url);
    let Some(info) = info else { return Ok(()) };
    if let (Some((username, token)), Some(host)) = (userinfo_credential(&info), url_host(&clean))
        && load_credential(state, id).await?.is_none()
    {
        store_credential(
            state,
            id,
            owner,
            &Credential {
                host,
                username,
                token,
            },
        )
        .await?;
    }
    git(dir, &["remote", "set-url", "origin", &clean]).await?;
    Ok(())
}

/// Runs git against the remote with the stored credential, if any.
async fn git_remote(dir: &FsPath, args: &[&str], cred: Option<&Credential>) -> AppResult<String> {
    let mut cmd = git_cmd(dir);
    cmd.env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        // An empty value resets the helper list (dropping the server user's helpers).
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "credential.helper")
        .env("GIT_CONFIG_VALUE_0", "")
        .env("GIT_CONFIG_KEY_1", "credential.helper")
        .env(
            "GIT_CONFIG_VALUE_1",
            if cred.is_some() { HELPER } else { "" },
        );
    if let Some(c) = cred {
        cmd.env("LIGHTLEAF_CRED_HOST", &c.host)
            .env("LIGHTLEAF_CRED_USER", &c.username)
            .env("LIGHTLEAF_CRED_TOKEN", &c.token);
    }
    cmd.args(args).stdin(std::process::Stdio::null());
    let out = tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), cmd.output())
        .await
        .map_err(|_| AppError::conflict(format!("git {} timed out", args[0])))?
        .map_err(|e| AppError::conflict(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        // Merge conflicts are reported on stdout, transport errors on stderr.
        let msg = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
        return Err(AppError::conflict(redact(msg.trim(), cred)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The configured branch, or the remote's default branch (Overleaf's is `master`).
async fn branch(dir: &FsPath, cred: Option<&Credential>) -> AppResult<String> {
    let b = git(dir, &["config", "--get", "lightleaf.remotebranch"])
        .await
        .unwrap_or_default();
    if !b.trim().is_empty() {
        return Ok(b.trim().to_string());
    }
    let out = git_remote(dir, &["ls-remote", "--symref", "origin", "HEAD"], cred).await?;
    Ok(out
        .lines()
        .find_map(|l| {
            l.strip_prefix("ref: refs/heads/")?
                .split_once('\t')
                .map(|(b, _)| b.to_string())
        })
        .unwrap_or_else(|| "main".into()))
}

fn valid_branch(b: &str) -> bool {
    !b.starts_with('-')
        && !b.contains("..")
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
}

pub async fn get_remote(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    let dir = state.cfg.project_dir(&id);
    let url = remote_url(&dir).await.map(|u| split_userinfo(&u).0);
    let branch = git(&dir, &["config", "--get", "lightleaf.remotebranch"])
        .await
        .unwrap_or_default();
    let pid = id.clone();
    let cred = state
        .db(move |c| {
            c.query_row(
                "SELECT rc.username, rc.updated_at, u.name FROM remote_credentials rc
                 LEFT JOIN users u ON u.id = rc.set_by WHERE rc.project_id = ?1",
                [&pid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
        })
        .await?;
    Ok(Json(json!({
        "url": url,
        "branch": if branch.trim().is_empty() { None } else { Some(branch.trim().to_string()) },
        "token": cred.map(|(username, at, by)| json!({ "username": username, "updatedAt": at, "setBy": by })),
    })))
}

#[derive(Deserialize)]
pub struct RemoteReq {
    url: String,
    /// Empty or absent: use the remote's default branch.
    branch: Option<String>,
    username: Option<String>,
    /// New token. Absent or empty keeps the stored one (if the host is unchanged).
    token: Option<String>,
    /// Delete the stored token.
    #[serde(default)]
    clear_token: bool,
}

pub async fn set_remote(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<RemoteReq>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Owner).await?;
    let dir = state.cfg.project_dir(&id);
    let lock = state.project_lock(&id);
    let _g = lock.lock().await;
    let url = req.url.trim();
    if url.is_empty() {
        let _ = git(&dir, &["remote", "remove", "origin"]).await;
        let _ = git(&dir, &["config", "--unset", "lightleaf.remotebranch"]).await;
        delete_credential(&state, &id).await?;
        return Ok(Json(json!({ "ok": true })));
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(AppError::bad("only http(s) remotes are supported"));
    }
    let branch = req.branch.as_deref().map(str::trim).unwrap_or("");
    if !branch.is_empty() && !valid_branch(branch) {
        return Err(AppError::bad("invalid branch name"));
    }
    let (clean, info) = split_userinfo(url);
    let host = url_host(&clean).ok_or_else(|| AppError::bad("invalid remote URL"))?;

    let mut new_cred = info.as_deref().and_then(userinfo_credential);
    if let Some(t) = req
        .token
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let username = new_cred
            .take()
            .map(|(u, _)| u)
            .unwrap_or_else(|| DEFAULT_USERNAME.into());
        new_cred = Some((username, t.to_string()));
    }
    if let Some((u, _)) = new_cred.as_mut()
        && let Some(name) = req
            .username
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
    {
        *u = name.to_string();
    }
    if new_cred.is_some() && !clean.starts_with("https://") {
        return Err(AppError::bad("a token can only be sent over https"));
    }
    if new_cred
        .as_ref()
        .is_some_and(|(u, t)| u.len() > 256 || t.len() > 4096 || t.contains(['\n', '\r']))
    {
        return Err(AppError::bad("invalid token"));
    }

    let _ = git(&dir, &["remote", "remove", "origin"]).await;
    git(&dir, &["remote", "add", "origin", &clean]).await?;
    if branch.is_empty() {
        let _ = git(&dir, &["config", "--unset", "lightleaf.remotebranch"]).await;
    } else {
        git(&dir, &["config", "lightleaf.remotebranch", branch]).await?;
    }

    let existing = load_credential(&state, &id).await.ok().flatten();
    match new_cred {
        Some((username, token)) => {
            store_credential(
                &state,
                &id,
                user.id,
                &Credential {
                    host,
                    username,
                    token,
                },
            )
            .await?
        }
        // A token is only ever sent to the host it was entered for.
        None if req.clear_token || existing.as_ref().is_some_and(|c| c.host != host) => {
            delete_credential(&state, &id).await?
        }
        None => {
            if let (Some(mut c), Some(name)) = (
                existing,
                req.username
                    .as_deref()
                    .map(str::trim)
                    .filter(|n| !n.is_empty()),
            ) {
                c.username = name.to_string();
                store_credential(&state, &id, user.id, &c).await?;
            }
        }
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Pull,
    Push,
    Sync,
}

async fn run(state: SharedState, user: User, id: String, op: Op) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Editor).await?;
    let dir = state.cfg.project_dir(&id);
    let lock = state.project_lock(&id);
    let _g = lock.lock().await;
    let pid = id.clone();
    let owner = state
        .db(move |c| {
            c.query_row("SELECT owner_id FROM projects WHERE id = ?1", [&pid], |r| {
                r.get::<_, i64>(0)
            })
        })
        .await?;
    migrate_legacy_url(&state, &id, &dir, owner).await?;
    if remote_url(&dir).await.is_none() {
        return Err(AppError::bad("no remote configured"));
    }
    let cred = load_credential(&state, &id).await?;
    let cred = cred.as_ref();
    let branch = branch(&dir, cred).await?;
    commit_pending(&state, &id, "Update from web editor", Some(&user)).await?;
    if op != Op::Push {
        let pull = git_remote(
            &dir,
            &[
                "pull",
                "--no-rebase",
                "--no-edit",
                "--allow-unrelated-histories",
                "origin",
                &branch,
            ],
            cred,
        )
        .await;
        if let Err(e) = pull {
            let _ = git(&dir, &["merge", "--abort"]).await;
            return Err(AppError::conflict(format!(
                "pull failed (merge conflicts are not resolved automatically): {}",
                e.1
            )));
        }
        if let Some(room) = state.hub.get(&id) {
            room.reload_from_disk().await;
        }
        state.touch_project(&id).await;
    }
    if op != Op::Pull {
        git_remote(
            &dir,
            &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
            cred,
        )
        .await
        .map_err(|e| AppError::conflict(format!("push failed (try pulling first): {}", e.1)))?;
    }
    Ok(Json(json!({ "ok": true, "at": now(), "branch": branch })))
}

pub async fn remote_pull(
    State(s): State<SharedState>,
    u: User,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    run(s, u, id, Op::Pull).await
}
pub async fn remote_push(
    State(s): State<SharedState>,
    u: User,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    run(s, u, id, Op::Push).await
}
/// Pull, then push: one button to bring both sides to the same commit.
pub async fn remote_sync(
    State(s): State<SharedState>,
    u: User,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    run(s, u, id, Op::Sync).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_userinfo() {
        assert_eq!(
            split_userinfo("https://git:olp_abc@git.overleaf.com/6a34"),
            (
                "https://git.overleaf.com/6a34".into(),
                Some("git:olp_abc".into())
            )
        );
        assert_eq!(
            split_userinfo("https://git.overleaf.com/6a34"),
            ("https://git.overleaf.com/6a34".into(), None)
        );
        // '@' after the path is not userinfo.
        assert_eq!(split_userinfo("https://h/a@b").1, None);
        assert_eq!(
            url_host("https://Git.Overleaf.com:443/x"),
            Some("git.overleaf.com:443".into())
        );
        assert_eq!(url_host("https:///x"), None);
    }

    #[test]
    fn userinfo_to_credential() {
        assert_eq!(
            userinfo_credential("git:olp_x"),
            Some(("git".into(), "olp_x".into()))
        );
        assert_eq!(
            userinfo_credential("ghp_x"),
            Some(("x-access-token".into(), "ghp_x".into()))
        );
        assert_eq!(
            userinfo_credential("a%40b:p%2Fq"),
            Some(("a@b".into(), "p/q".into()))
        );
        assert_eq!(userinfo_credential(""), None);
    }

    #[test]
    fn redacts_tokens_and_urls() {
        let c = Credential {
            host: "h".into(),
            username: "git".into(),
            token: "olp_secret".into(),
        };
        let m = redact(
            "fatal: unable to access 'https://git:olp_secret@h/x': olp_secret",
            Some(&c),
        );
        assert!(!m.contains("olp_secret"), "{m}");
        assert_eq!(
            redact("see https://u:p@h/x now", None),
            "see https://h/x now"
        );
    }

    #[test]
    fn branch_names() {
        assert!(valid_branch("master") && valid_branch("feat/x-1"));
        assert!(
            !valid_branch("-x")
                && !valid_branch("a..b")
                && !valid_branch("a b")
                && !valid_branch("a;b")
        );
    }
}
