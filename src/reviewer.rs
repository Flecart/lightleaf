//! Optional integration with the AI review service (`lightleaf-reviewer`, see `reviewer/`).
//!
//! The reviewer is a separate process. Lightleaf only:
//! 1. issues a short-lived git token restricted to one project,
//! 2. asks the reviewer to start a job (the reviewer clones the project over git, writes its
//!    comments into the LaTeX source as `\ANG[AI: …]{…}` macros, and pushes a commit), and
//! 3. relays job status to the browser.
//!
//! Nothing else in Lightleaf depends on this module. When `LIGHTLEAF_REVIEWER_URL` is unset the
//! routes answer 404 and the UI hides the feature; when the reviewer is down or slow they answer
//! 503 within a few seconds. Editing, compiling and git keep working either way.

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::{Role, SharedState},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest reviewer response we accept (job status with comment summaries).
const MAX_RESPONSE: usize = 4 * 1024 * 1024;

/// Reviewer connection settings, present only when the integration is configured.
#[derive(Clone, Debug)]
pub struct ReviewerConfig {
    /// `http://host:port` of the reviewer service (internal network; no TLS).
    pub url: String,
    /// Shared secret sent as `Authorization: Bearer …` to the reviewer.
    pub secret: String,
    /// Base URL the reviewer uses to reach this server's git endpoint.
    pub git_base: String,
    /// Lifetime of the project token handed to the reviewer.
    pub token_ttl_secs: i64,
}

fn unavailable(e: impl std::fmt::Display) -> AppError {
    tracing::warn!("reviewer unavailable: {e}");
    AppError(
        StatusCode::SERVICE_UNAVAILABLE,
        "The review service is unavailable right now. Editing and compiling are not affected."
            .into(),
    )
}

fn cfg(state: &SharedState) -> AppResult<&ReviewerConfig> {
    state.cfg.reviewer.as_ref().ok_or_else(AppError::not_found)
}

/// Minimal HTTP/1.1 client for the internal reviewer API (plain `http://`, JSON bodies).
async fn call(
    rc: &ReviewerConfig,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> AppResult<(u16, Value)> {
    let fut = async {
        let rest = rc
            .url
            .strip_prefix("http://")
            .ok_or_else(|| anyhow::anyhow!("LIGHTLEAF_REVIEWER_URL must start with http://"))?;
        let (hostport, prefix) = match rest.split_once('/') {
            Some((h, p)) => (h, format!("/{}", p.trim_end_matches('/'))),
            None => (rest, String::new()),
        };
        let addr = if hostport.contains(':') {
            hostport.to_string()
        } else {
            format!("{hostport}:80")
        };
        let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| anyhow::anyhow!("connect timeout"))??;
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let req = format!(
            "{method} {prefix}{path} HTTP/1.1\r\nHost: {hostport}\r\nAuthorization: Bearer {}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            rc.secret,
            payload.len()
        );
        stream.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        (&mut stream)
            .take(MAX_RESPONSE as u64)
            .read_to_end(&mut buf)
            .await?;
        parse_response(&buf)
    };
    match tokio::time::timeout(REQUEST_TIMEOUT, fut).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(e)) => Err(unavailable(e)),
        Err(_) => Err(unavailable("request timed out")),
    }
}

fn parse_response(buf: &[u8]) -> anyhow::Result<(u16, Value)> {
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("malformed response"))?;
    let head = std::str::from_utf8(&buf[..split])?;
    let mut body = buf[split + 4..].to_vec();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("malformed status line"))?;
    let chunked = head.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    if chunked {
        body = dechunk(&body)?;
    }
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)?
    };
    Ok((status, value))
}

fn dechunk(mut data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = data
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| anyhow::anyhow!("bad chunk"))?;
        let size_str = std::str::from_utf8(&data[..eol])?;
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("").trim(), 16)?;
        data = &data[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        anyhow::ensure!(data.len() >= size, "truncated chunk");
        out.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or(&[]);
    }
}

/// Forwards the reviewer's answer: 2xx bodies as-is, errors with the reviewer's message.
fn relay((status, body): (u16, Value)) -> AppResult<Json<Value>> {
    if (200..300).contains(&status) {
        return Ok(Json(body));
    }
    let msg = body["error"]
        .as_str()
        .unwrap_or("review service error")
        .to_string();
    let code = match status {
        400 | 404 | 409 | 422 | 429 => {
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY)
        }
        _ => StatusCode::BAD_GATEWAY,
    };
    Err(AppError(code, msg))
}

#[derive(Deserialize, Default)]
pub struct StartReq {
    /// Target venue, e.g. "NeurIPS" (free text; empty = general academic standards).
    #[serde(default)]
    venue: String,
    /// Optional extra instructions from the author ("focus on the methods section").
    #[serde(default)]
    focus: String,
}

async fn job_payload(
    state: &SharedState,
    user: &User,
    id: &str,
    kind: &str,
    opts: Value,
) -> AppResult<Value> {
    let rc = cfg(state)?;
    let pid = id.to_string();
    let (name, compiler, main_file): (String, String, String) = state
        .db(move |c| {
            c.query_row(
                "SELECT name, compiler, main_file FROM projects WHERE id = ?1",
                [pid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        })
        .await?
        .ok_or_else(AppError::not_found)?;
    let token =
        crate::auth::issue_project_token(state, user.id, id, "reviewer", rc.token_ttl_secs).await?;
    Ok(json!({
        "kind": kind,
        "projectId": id,
        "projectName": name,
        "compiler": compiler,
        "mainFile": main_file,
        "gitUrl": format!("{}/git/{id}", rc.git_base.trim_end_matches('/')),
        "token": token,
        "requestedBy": { "name": user.name, "email": user.email },
        "options": opts,
    }))
}

/// Starts an AI review of the project. The comments arrive later as a git commit.
pub async fn start(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    body: Option<Json<StartReq>>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Editor).await?;
    crate::collab::flush_project(&state, &id).await?;
    let req = body.map(|b| b.0).unwrap_or_default();
    let opts = json!({
        "venue": req.venue.chars().take(100).collect::<String>(),
        "focus": req.focus.chars().take(2000).collect::<String>(),
    });
    let payload = job_payload(&state, &user, &id, "review", opts).await?;
    relay(call(cfg(&state)?, "POST", "/v1/jobs", Some(&payload)).await?)
}

/// Removes every AI comment (`\ANG[AI…]{…}`) from the project, as one commit.
pub async fn clear(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Editor).await?;
    crate::collab::flush_project(&state, &id).await?;
    let payload = job_payload(&state, &user, &id, "clear", json!({})).await?;
    relay(call(cfg(&state)?, "POST", "/v1/jobs", Some(&payload)).await?)
}

/// Status of the project's most recent job.
pub async fn latest(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    relay(
        call(
            cfg(&state)?,
            "GET",
            &format!("/v1/projects/{id}/latest"),
            None,
        )
        .await?,
    )
}

/// Status of one job of this project.
pub async fn status(
    State(state): State<SharedState>,
    user: User,
    Path((id, job)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    if !job.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(AppError::bad("invalid job id"));
    }
    relay(
        call(
            cfg(&state)?,
            "GET",
            &format!("/v1/projects/{id}/jobs/{job}"),
            None,
        )
        .await?,
    )
}

/// Whether the reviewer answers its health check (for the UI and /metrics-style probes).
pub async fn health(State(state): State<SharedState>, _user: User) -> AppResult<Json<Value>> {
    match call(cfg(&state)?, "GET", "/health", None).await {
        Ok((200, body)) => Ok(Json(json!({ "ok": true, "reviewer": body }))),
        Ok((_, body)) => Ok(Json(json!({ "ok": false, "reviewer": body }))),
        Err(e) => Ok(Json(json!({ "ok": false, "error": e.1 }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_chunked_responses() {
        let r = b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\n{\"ok\":true}";
        assert_eq!(parse_response(r).unwrap(), (200, json!({"ok": true})));
        let r = b"HTTP/1.1 404 Not Found\r\ntransfer-encoding: chunked\r\n\r\n5\r\n{\"a\":\r\n2\r\n1}\r\n0\r\n\r\n";
        assert_eq!(parse_response(r).unwrap(), (404, json!({"a": 1})));
        assert!(parse_response(b"garbage").is_err());
    }
}
