use crate::{
    auth::User,
    collab::flush_project,
    error::{AppError, AppResult},
    state::{Role, SharedState},
    util::{is_text, is_text_ext, join_safe},
};
use axum::{
    Json,
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path as FsPath, PathBuf};

#[derive(Serialize)]
pub struct Entry {
    path: String,
    kind: &'static str,
    size: u64,
    text: bool,
}

/// Walks the project tree (excluding `.git`) and returns all entries, sorted.
pub fn walk(root: &FsPath) -> Vec<(String, PathBuf, std::fs::Metadata)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if dir == root && name == ".git" {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
            if meta.file_type().is_symlink() {
                continue;
            }
            let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            if meta.is_dir() {
                stack.push(e.path());
            }
            out.push((rel, e.path(), meta));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub async fn tree(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
) -> AppResult<Json<Vec<Entry>>> {
    state.require(&id, user.id, Role::Viewer).await?;
    let root = state.cfg.project_dir(&id);
    let entries = tokio::task::spawn_blocking(move || {
        walk(&root)
            .into_iter()
            .map(|(rel, _, meta)| Entry {
                text: meta.is_file() && is_text_ext(&rel),
                kind: if meta.is_dir() { "dir" } else { "file" },
                size: meta.len(),
                path: rel,
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(anyhow::Error::from)?;
    Ok(Json(entries))
}

#[derive(Deserialize)]
pub struct PathQuery {
    path: String,
}

/// Returns the raw bytes of a file (used for images/PDF previews and downloads).
pub async fn read(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> AppResult<Response> {
    state.require(&id, user.id, Role::Viewer).await?;
    flush_project(&state, &id).await?;
    let (rel, full) = join_safe(&state.cfg.project_dir(&id), &q.path)?;
    let meta = tokio::fs::symlink_metadata(&full).await.map_err(|_| AppError::not_found())?;
    if !meta.is_file() {
        return Err(AppError::not_found());
    }
    let bytes = tokio::fs::read(&full).await?;
    let mime = mime_guess::from_path(&rel).first_or_octet_stream();
    Ok(([(header::CONTENT_TYPE, mime.to_string()), (header::CACHE_CONTROL, "no-cache".into())], bytes).into_response())
}

#[derive(Deserialize)]
pub struct CreateReq {
    path: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

pub async fn create(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<CreateReq>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Editor).await?;
    let (rel, full) = join_safe(&state.cfg.project_dir(&id), &req.path)?;
    if tokio::fs::symlink_metadata(&full).await.is_ok() {
        return Err(AppError::conflict("a file with that name already exists"));
    }
    if req.kind.as_deref() == Some("dir") {
        tokio::fs::create_dir_all(&full).await?;
    } else {
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&full, req.content.unwrap_or_default()).await?;
    }
    notify_tree(&state, &id);
    Ok(Json(json!({ "path": rel })))
}

#[derive(Deserialize)]
pub struct RenameReq {
    from: String,
    to: String,
}

pub async fn rename(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Json(req): Json<RenameReq>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Editor).await?;
    let root = state.cfg.project_dir(&id);
    let (from, from_full) = join_safe(&root, &req.from)?;
    let (to, to_full) = join_safe(&root, &req.to)?;
    if to.starts_with(&format!("{from}/")) {
        return Err(AppError::bad("cannot move a folder into itself"));
    }
    let lock = state.project_lock(&id);
    let _g = lock.lock().await;
    if let Some(room) = state.hub.get(&id) {
        room.flush_locked().await?;
        room.forget(&from);
    }
    if tokio::fs::symlink_metadata(&to_full).await.is_ok() {
        return Err(AppError::conflict("target already exists"));
    }
    if let Some(parent) = to_full.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::rename(&from_full, &to_full).await.map_err(|_| AppError::not_found())?;
    // Keep the main file setting valid when it gets renamed.
    let (pid, f, t) = (id.clone(), from.clone(), to.clone());
    state
        .db(move |c| c.execute("UPDATE projects SET main_file = ?3 WHERE id = ?1 AND main_file = ?2", rusqlite::params![pid, f, t]))
        .await?;
    if let Some(room) = state.hub.get(&id) {
        room.broadcast_json(json!({ "type": "renamed", "from": from, "to": to }));
    }
    notify_tree(&state, &id);
    Ok(Json(json!({ "path": to })))
}

pub async fn remove(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Editor).await?;
    let (rel, full) = join_safe(&state.cfg.project_dir(&id), &q.path)?;
    let lock = state.project_lock(&id);
    let _g = lock.lock().await;
    if let Some(room) = state.hub.get(&id) {
        room.forget(&rel);
    }
    let meta = tokio::fs::symlink_metadata(&full).await.map_err(|_| AppError::not_found())?;
    if meta.is_dir() {
        tokio::fs::remove_dir_all(&full).await?;
    } else {
        tokio::fs::remove_file(&full).await?;
    }
    if let Some(room) = state.hub.get(&id) {
        room.broadcast_json(json!({ "type": "deleted", "path": rel }));
    }
    notify_tree(&state, &id);
    Ok(Json(json!({ "ok": true })))
}

pub async fn upload(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    mut mp: Multipart,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Editor).await?;
    let root = state.cfg.project_dir(&id);
    let mut folder = String::new();
    let mut saved = Vec::new();
    while let Some(field) = mp.next_field().await.map_err(|e| AppError::bad(e.to_string()))? {
        if field.name() == Some("folder") {
            folder = field.text().await.unwrap_or_default();
            continue;
        }
        let Some(name) = field.file_name().map(|s| s.to_string()) else { continue };
        let rel = if folder.trim().is_empty() { name } else { format!("{}/{}", folder.trim_end_matches('/'), name) };
        let (rel, full) = join_safe(&root, &rel)?;
        let data = field.bytes().await.map_err(|e| AppError::bad(e.to_string()))?;
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let lock = state.project_lock(&id);
        let _g = lock.lock().await;
        tokio::fs::write(&full, &data).await?;
        if let Some(room) = state.hub.get(&id) {
            if is_text(&data) {
                room.reload_from_disk().await;
            } else {
                room.forget(&rel);
            }
        }
        saved.push(rel);
    }
    notify_tree(&state, &id);
    Ok(Json(json!({ "saved": saved })))
}

pub fn notify_tree(state: &SharedState, id: &str) {
    if let Some(room) = state.hub.get(id) {
        room.broadcast_json(json!({ "type": "tree" }));
    }
}

pub async fn download_zip(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
) -> AppResult<Response> {
    state.require(&id, user.id, Role::Viewer).await?;
    flush_project(&state, &id).await?;
    let pid = id.clone();
    let name: String = state.db(move |c| c.query_row("SELECT name FROM projects WHERE id = ?1", [pid], |r| r.get(0))).await?;
    let root = state.cfg.project_dir(&id);
    // Leave out the .gitignore Lightleaf generated itself, so the archive matches Overleaf's.
    let bytes = tokio::task::spawn_blocking(move || {
        crate::overleaf::export_zip(&root, |rel, data| rel == ".gitignore" && data == crate::projects::GITIGNORE.as_bytes())
    })
    .await
    .map_err(anyhow::Error::from)??;
    // Overleaf names downloads `Project_Name.zip`.
    let ascii: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let utf8: String = name
        .replace(' ', "_")
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{ascii}.zip\"; filename*=UTF-8''{utf8}.zip")),
        ],
        Body::from(bytes),
    )
        .into_response())
}
