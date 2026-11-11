//! Git integration: every project *is* a git repository.
//!
//! * Smart-HTTP endpoints so users can `git clone`/`pull`/`push` a project, authenticating
//!   with a personal access token (like Overleaf's git bridge).
//! * Web edits are auto-committed whenever a git client fetches, and on explicit snapshots.
//! * History browsing, diffs and restoring old versions.
//! * Sync with an external remote lives in `remote.rs`.

use crate::{
    auth::{User, user_from_basic},
    collab::Room,
    error::{AppError, AppResult},
    state::{Role, SharedState},
    util::clean_rel_path,
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use std::{io::Read, path::Path as FsPath, process::Stdio, sync::Arc};
use tokio::{io::AsyncWriteExt, process::Command};
use tokio_util::io::ReaderStream;

const BOT_NAME: &str = "Lightleaf";
const BOT_EMAIL: &str = "lightleaf@localhost";

pub(crate) fn git_cmd(dir: &FsPath) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_COMMITTER_NAME", BOT_NAME)
        .env("GIT_COMMITTER_EMAIL", BOT_EMAIL)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .arg("-c")
        .arg("protocol.file.allow=never")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .kill_on_drop(true);
    c
}

/// Runs git and returns stdout, failing with stderr on a non-zero exit.
pub async fn git(dir: &FsPath, args: &[&str]) -> anyhow::Result<String> {
    let out = git_cmd(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub async fn init_repo(dir: &FsPath) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    git(dir, &["init", "-q", "-b", "main"]).await?;
    for (k, v) in [
        ("receive.denyCurrentBranch", "updateInstead"),
        ("core.symlinks", "false"),
        ("core.quotepath", "false"),
        ("user.name", BOT_NAME),
        ("user.email", BOT_EMAIL),
        ("gc.auto", "256"),
    ] {
        git(dir, &["config", k, v]).await?;
    }
    Ok(())
}

/// Commits all working-tree changes. The caller must hold the project lock.
/// Returns `true` if a commit was created.
pub async fn commit_all(
    dir: &FsPath,
    room: Option<&Arc<Room>>,
    message: &str,
    actor: Option<&User>,
) -> anyhow::Result<bool> {
    if let Some(room) = room {
        room.flush_locked()
            .await
            .map_err(|e| anyhow::anyhow!(e.1))?;
    }
    git(dir, &["add", "-A"]).await?;
    let status = git(dir, &["status", "--porcelain"]).await?;
    if status.trim().is_empty() {
        return Ok(false);
    }
    let mut editors = room.map(|r| r.take_editors()).unwrap_or_default();
    if let Some(u) = actor
        && !editors.iter().any(|(_, e)| *e == u.email)
    {
        editors.insert(0, (u.name.clone(), u.email.clone()));
    }
    let (an, ae) = editors
        .first()
        .cloned()
        .unwrap_or((BOT_NAME.into(), BOT_EMAIL.into()));
    let mut msg = message.to_string();
    if editors.len() > 1 {
        msg.push_str("\n\n");
        for (n, e) in &editors[1..] {
            msg.push_str(&format!("Co-authored-by: {n} <{e}>\n"));
        }
    }
    let out = git_cmd(dir)
        .args(["commit", "-q", "--no-verify", "-m", &msg])
        .env("GIT_AUTHOR_NAME", an)
        .env("GIT_AUTHOR_EMAIL", ae)
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!("git commit: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(true)
}

pub(crate) async fn commit_pending(
    state: &SharedState,
    id: &str,
    message: &str,
    actor: Option<&User>,
) -> AppResult<bool> {
    let room = state.hub.get(id);
    Ok(commit_all(&state.cfg.project_dir(id), room.as_ref(), message, actor).await?)
}

// ---------------------------------------------------------------------------------------------
// Smart HTTP
// ---------------------------------------------------------------------------------------------

fn need_auth() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"Lightleaf git\"")],
        "authentication required\n",
    )
        .into_response()
}

#[allow(clippy::result_large_err)]
async fn git_auth(
    state: &SharedState,
    headers: &HeaderMap,
    id: &str,
    min: Role,
) -> Result<User, Response> {
    let user = match user_from_basic(state, headers).await {
        Ok(Some(t)) if t.project.as_deref().is_none_or(|p| p == id) => t.user,
        // A project-scoped token used for another project: behave as if it did not exist.
        Ok(Some(_)) | Ok(None) => return Err(need_auth()),
        Err(e) => return Err(e.into_response()),
    };
    match state.role(id, user.id).await {
        Ok(r) if r >= min => Ok(user),
        Ok(_) => Err((
            StatusCode::FORBIDDEN,
            "you only have read access to this project\n",
        )
            .into_response()),
        Err(_) => Err((StatusCode::NOT_FOUND, "repository not found\n").into_response()),
    }
}

fn project_id(raw: &str) -> String {
    raw.trim_end_matches(".git").to_string()
}

fn pkt_line(s: &str) -> Vec<u8> {
    let mut v = format!("{:04x}", s.len() + 4).into_bytes();
    v.extend_from_slice(s.as_bytes());
    v
}

#[derive(Deserialize)]
pub struct ServiceQuery {
    service: Option<String>,
}

pub async fn info_refs(
    State(state): State<SharedState>,
    Path(raw): Path<String>,
    Query(q): Query<ServiceQuery>,
    headers: HeaderMap,
) -> Response {
    let id = project_id(&raw);
    let service = match q.service.as_deref() {
        Some("git-upload-pack") => "upload-pack",
        Some("git-receive-pack") => "receive-pack",
        _ => {
            return (
                StatusCode::FORBIDDEN,
                "dumb http protocol is not supported\n",
            )
                .into_response();
        }
    };
    let min = if service == "receive-pack" {
        Role::Editor
    } else {
        Role::Viewer
    };
    if let Err(r) = git_auth(&state, &headers, &id, min).await {
        return r;
    }
    // Make sure the latest web edits are visible to the git client.
    {
        let lock = state.project_lock(&id);
        let _g = lock.lock().await;
        if let Err(e) = commit_pending(&state, &id, "Update from web editor", None).await {
            return e.into_response();
        }
    }
    let dir = state.cfg.project_dir(&id);
    let mut cmd = git_cmd(&dir);
    cmd.args([service, "--stateless-rpc", "--advertise-refs", "."]);
    if let Some(p) = headers.get("git-protocol").and_then(|v| v.to_str().ok()) {
        cmd.env("GIT_PROTOCOL", p);
    }
    let out = match cmd.output().await {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => return (StatusCode::INTERNAL_SERVER_ERROR, o.stderr).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut body = Vec::new();
    let is_v2 = headers
        .get("git-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("version=2"));
    if !is_v2 {
        body.extend(pkt_line(&format!("# service=git-{service}\n")));
        body.extend_from_slice(b"0000");
    }
    body.extend(out);
    (
        [
            (
                header::CONTENT_TYPE,
                format!("application/x-git-{service}-advertisement"),
            ),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        body,
    )
        .into_response()
}

fn decode_body(headers: &HeaderMap, body: Bytes) -> std::io::Result<Vec<u8>> {
    if headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        == Some("gzip")
    {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&body[..]).read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(body.to_vec())
    }
}

pub async fn upload_pack(
    State(state): State<SharedState>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let id = project_id(&raw);
    if let Err(r) = git_auth(&state, &headers, &id, Role::Viewer).await {
        return r;
    }
    let Ok(input) = decode_body(&headers, body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let mut cmd = git_cmd(&state.cfg.project_dir(&id));
    cmd.args(["upload-pack", "--stateless-rpc", "."])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(p) = headers.get("git-protocol").and_then(|v| v.to_str().ok()) {
        cmd.env("GIT_PROTOCOL", p);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut stdin = child.stdin.take().unwrap();
    tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
    });
    let stdout = child.stdout.take().unwrap();
    // Reap the child once the stream is done.
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    (
        [
            (header::CONTENT_TYPE, "application/x-git-upload-pack-result"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(ReaderStream::new(stdout)),
    )
        .into_response()
}

pub async fn receive_pack(
    State(state): State<SharedState>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let id = project_id(&raw);
    let user = match git_auth(&state, &headers, &id, Role::Editor).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Ok(input) = decode_body(&headers, body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let lock = state.project_lock(&id);
    let _g = lock.lock().await;
    // The working tree must be clean for `updateInstead` to accept the push.
    if let Err(e) = commit_pending(&state, &id, "Update from web editor", None).await {
        return e.into_response();
    }
    let dir = state.cfg.project_dir(&id);
    let mut cmd = git_cmd(&dir);
    cmd.args(["receive-pack", "--stateless-rpc", "."])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut stdin = child.stdin.take().unwrap();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
    });
    let out = match child.wait_with_output().await {
        Ok(o) => o,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let _ = writer.await;
    tracing::info!("git push to {id} by {}", user.email);
    if let Some(room) = state.hub.get(&id) {
        room.reload_from_disk().await;
    }
    state.touch_project(&id).await;
    (
        [
            (
                header::CONTENT_TYPE,
                "application/x-git-receive-pack-result",
            ),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        out.stdout,
    )
        .into_response()
}

// ---------------------------------------------------------------------------------------------
// History API
// ---------------------------------------------------------------------------------------------

fn valid_sha(s: &str) -> AppResult<()> {
    if (4..=40).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(AppError::bad("invalid revision"))
    }
}

pub async fn history(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    let dir = state.cfg.project_dir(&id);
    let log = git(
        &dir,
        &[
            "log",
            "-n",
            "300",
            "--shortstat",
            "--format=%x1e%H%x1f%an%x1f%ae%x1f%at%x1f%s",
        ],
    )
    .await
    .unwrap_or_default();
    let commits: Vec<_> = log
        .split('\x1e')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|chunk| {
            let mut lines = chunk.trim().lines();
            let head = lines.next()?;
            let f: Vec<&str> = head.split('\x1f').collect();
            if f.len() < 5 {
                return None;
            }
            let stat = lines.find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            Some(json!({ "sha": f[0], "author": f[1], "email": f[2], "time": f[3].parse::<i64>().unwrap_or(0),
                         "message": f[4], "stat": stat }))
        })
        .collect();
    let pending = {
        let room = state.hub.get(&id);
        let lock = state.project_lock(&id);
        let _g = lock.lock().await;
        if let Some(r) = &room {
            r.flush_locked().await?;
        }
        !git(&dir, &["status", "--porcelain"])
            .await
            .unwrap_or_default()
            .trim()
            .is_empty()
    };
    Ok(Json(json!({ "commits": commits, "pending": pending })))
}

#[derive(Deserialize)]
pub struct SnapshotReq {
    message: Option<String>,
}

pub async fn snapshot(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<SnapshotReq>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Editor).await?;
    let msg = req
        .message
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "Snapshot from web editor".into());
    let lock = state.project_lock(&id);
    let _g = lock.lock().await;
    let created = commit_pending(&state, &id, msg.trim(), Some(&user)).await?;
    Ok(Json(json!({ "created": created })))
}

pub async fn show_commit(
    State(state): State<SharedState>,
    user: User,
    Path((id, sha)): Path<(String, String)>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    valid_sha(&sha)?;
    let dir = state.cfg.project_dir(&id);
    let numstat = git(
        &dir,
        &["show", "--numstat", "--format=", "--no-renames", &sha],
    )
    .await
    .map_err(|_| AppError::not_found())?;
    let files: Vec<_> = numstat
        .lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let (a, d, f) = (p.next()?, p.next()?, p.next()?);
            Some(json!({ "path": f, "added": a.parse::<i64>().ok(), "removed": d.parse::<i64>().ok() }))
        })
        .collect();
    let mut patch = git(
        &dir,
        &[
            "show",
            "--format=",
            "--no-renames",
            "--patch",
            "--no-color",
            &sha,
        ],
    )
    .await?;
    const MAX: usize = 512 * 1024;
    if patch.len() > MAX {
        let mut cut = MAX;
        while !patch.is_char_boundary(cut) {
            cut -= 1;
        }
        patch.truncate(cut);
        patch.push_str("\n... diff truncated ...\n");
    }
    let body = git(&dir, &["show", "-s", "--format=%B", &sha])
        .await
        .unwrap_or_default();
    Ok(Json(
        json!({ "sha": sha, "files": files, "patch": patch, "message": body.trim() }),
    ))
}

#[derive(Deserialize)]
pub struct RestoreReq {
    path: Option<String>,
}

pub async fn restore_file(
    State(state): State<SharedState>,
    user: User,
    Path((id, sha)): Path<(String, String)>,
    Json(req): Json<RestoreReq>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Editor).await?;
    valid_sha(&sha)?;
    let dir = state.cfg.project_dir(&id);
    let lock = state.project_lock(&id);
    let _g = lock.lock().await;
    commit_pending(&state, &id, "Update from web editor", None).await?;
    let short = &sha[..sha.len().min(8)];
    match req.path.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => {
            let rel = clean_rel_path(p)?;
            git(&dir, &["checkout", &sha, "--", &rel])
                .await
                .map_err(|e| AppError::bad(e.to_string()))?;
            commit_all(
                &dir,
                None,
                &format!("Restore {rel} from {short}"),
                Some(&user),
            )
            .await?;
        }
        None => {
            git(&dir, &["read-tree", "-u", "--reset", &sha])
                .await
                .map_err(|e| AppError::bad(e.to_string()))?;
            commit_all(
                &dir,
                None,
                &format!("Restore project to {short}"),
                Some(&user),
            )
            .await?;
        }
    }
    if let Some(room) = state.hub.get(&id) {
        room.reload_from_disk().await;
    }
    Ok(Json(json!({ "ok": true })))
}
