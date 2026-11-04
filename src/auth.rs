use crate::{
    config::Signup,
    error::{AppError, AppResult},
    state::{AppState, SharedState},
    util::{now, random_token, sha256_hex},
};
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use axum::{
    Json,
    extract::{FromRequestParts, Path, State},
    http::{HeaderMap, HeaderValue, header, request::Parts},
    response::{IntoResponse, Response},
};
use base64::Engine;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

const SESSION_COOKIE: &str = "ll_session";
const SESSION_DAYS: i64 = 30;

#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
}

fn row_to_user(r: &rusqlite::Row) -> rusqlite::Result<User> {
    Ok(User { id: r.get(0)?, email: r.get(1)?, name: r.get(2)?, is_admin: r.get::<_, i64>(3)? != 0 })
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).find_map(|v| {
        v.split(';').find_map(|kv| {
            let (k, val) = kv.trim().split_once('=')?;
            (k == name).then(|| val.to_string())
        })
    })
}

pub async fn user_from_session(state: &AppState, headers: &HeaderMap) -> AppResult<Option<User>> {
    let Some(token) = cookie_value(headers, SESSION_COOKIE) else { return Ok(None) };
    let hash = sha256_hex(&token);
    state
        .db(move |c| {
            c.query_row(
                "SELECT u.id, u.email, u.name, u.is_admin FROM sessions s JOIN users u ON u.id = s.user_id
                 WHERE s.token_hash = ?1 AND s.expires_at > ?2",
                params![hash, now()],
                row_to_user,
            )
            .optional()
        })
        .await
}

/// Authenticates git clients via HTTP Basic auth: any username + a personal access token
/// as the password (like Overleaf's git bridge).
pub async fn user_from_basic(state: &AppState, headers: &HeaderMap) -> AppResult<Option<User>> {
    let Some(v) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else { return Ok(None) };
    let Some(b64) = v.strip_prefix("Basic ") else { return Ok(None) };
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else { return Ok(None) };
    let raw = String::from_utf8_lossy(&raw).to_string();
    let Some((_user, secret)) = raw.split_once(':') else { return Ok(None) };
    let hash = sha256_hex(secret);
    state
        .db(move |c| {
            let u = c
                .query_row(
                    "SELECT u.id, u.email, u.name, u.is_admin FROM tokens t JOIN users u ON u.id = t.user_id
                     WHERE t.token_hash = ?1",
                    params![hash],
                    row_to_user,
                )
                .optional()?;
            if u.is_some() {
                c.execute("UPDATE tokens SET last_used = ?2 WHERE token_hash = ?1", params![hash, now()])?;
            }
            Ok(u)
        })
        .await
}

impl FromRequestParts<SharedState> for User {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &SharedState) -> Result<Self, Self::Rejection> {
        user_from_session(state, &parts.headers).await?.ok_or_else(AppError::unauthorized)
    }
}

async fn hash_password(pw: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(pw.as_bytes())
            .map(|h| h.to_string())
            .map_err(|e| anyhow::anyhow!("hash error: {e}"))
    })
    .await
    .map_err(anyhow::Error::from)?
    .map_err(Into::into)
}

async fn verify_password(pw: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok()).unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

fn validate_new_account(email: &str, name: &str, password: &str) -> AppResult<()> {
    if !email.contains('@') || email.len() > 200 {
        return Err(AppError::bad("invalid email address"));
    }
    if name.trim().is_empty() || name.len() > 100 {
        return Err(AppError::bad("invalid name"));
    }
    if password.len() < 8 {
        return Err(AppError::bad("password must be at least 8 characters"));
    }
    Ok(())
}

async fn insert_user(state: &AppState, email: &str, name: &str, password: &str, admin: Option<bool>) -> AppResult<i64> {
    validate_new_account(email, name, password)?;
    let hash = hash_password(password.to_string()).await?;
    let (email, name) = (email.trim().to_string(), name.trim().to_string());
    state
        .db(move |c| {
            let count: i64 = c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
            let is_admin = admin.unwrap_or(count == 0);
            let exists: Option<i64> =
                c.query_row("SELECT id FROM users WHERE email = ?1", [&email], |r| r.get(0)).optional()?;
            if exists.is_some() {
                return Ok(-1);
            }
            c.execute(
                "INSERT INTO users (email, name, password_hash, is_admin, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![email, name, hash, is_admin as i64, now()],
            )?;
            Ok(c.last_insert_rowid())
        })
        .await
        .and_then(|id| if id < 0 { Err(AppError::conflict("an account with this email already exists")) } else { Ok(id) })
}

pub async fn bootstrap_admin(state: &AppState, email: &str, password: &str) -> anyhow::Result<()> {
    let e = email.to_string();
    let exists: bool = state
        .db(move |c| c.query_row("SELECT COUNT(*) FROM users WHERE email = ?1", [e], |r| r.get::<_, i64>(0)))
        .await
        .map_err(|e| anyhow::anyhow!(e.1))?
        > 0;
    if !exists {
        insert_user(state, email, "Admin", password, Some(true)).await.map_err(|e| anyhow::anyhow!(e.1))?;
        tracing::info!("created bootstrap admin {email}");
    }
    Ok(())
}

async fn start_session(state: &AppState, user_id: i64) -> AppResult<Response> {
    let token = random_token(32);
    let hash = sha256_hex(&token);
    let expires = now() + SESSION_DAYS * 86400;
    state
        .db(move |c| {
            c.execute("DELETE FROM sessions WHERE expires_at < ?1", [now()])?;
            c.execute("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)", params![
                hash, user_id, expires
            ])
        })
        .await?;
    let secure = if state.cfg.secure_cookies { "; Secure" } else { "" };
    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{secure}",
        SESSION_DAYS * 86400
    );
    let mut res = Json(json!({ "ok": true })).into_response();
    res.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).map_err(anyhow::Error::from)?);
    Ok(res)
}

pub async fn public_config(State(state): State<SharedState>) -> AppResult<Json<serde_json::Value>> {
    let users: i64 = state.db(|c| c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))).await?;
    let compilers = crate::compile::available_compilers();
    Ok(Json(json!({
        "siteName": state.cfg.site_name,
        "signup": state.cfg.signup == Signup::Open || users == 0,
        "firstUser": users == 0,
        "compilers": compilers,
    })))
}

#[derive(Deserialize)]
pub struct RegisterReq {
    email: String,
    name: String,
    password: String,
}

pub async fn register(State(state): State<SharedState>, Json(req): Json<RegisterReq>) -> AppResult<Response> {
    let users: i64 = state.db(|c| c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))).await?;
    if state.cfg.signup == Signup::Closed && users > 0 {
        return Err(AppError::forbidden());
    }
    let id = insert_user(&state, &req.email, &req.name, &req.password, None).await?;
    start_session(&state, id).await
}

#[derive(Deserialize)]
pub struct LoginReq {
    email: String,
    password: String,
}

pub async fn login(State(state): State<SharedState>, Json(req): Json<LoginReq>) -> AppResult<Response> {
    let email = req.email.trim().to_string();
    let row: Option<(i64, String)> = state
        .db(move |c| {
            c.query_row("SELECT id, password_hash FROM users WHERE email = ?1", [email], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
        })
        .await?;
    if let Some((id, hash)) = row {
        if verify_password(req.password, hash).await {
            return start_session(&state, id).await;
        }
    }
    Err(AppError(axum::http::StatusCode::UNAUTHORIZED, "invalid email or password".into()))
}

pub async fn logout(State(state): State<SharedState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(token) = cookie_value(&headers, SESSION_COOKIE) {
        let hash = sha256_hex(&token);
        state.db(move |c| c.execute("DELETE FROM sessions WHERE token_hash = ?1", [hash])).await?;
    }
    let mut res = Json(json!({ "ok": true })).into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("ll_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    Ok(res)
}

pub async fn me(user: User) -> Json<User> {
    Json(user)
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    current: String,
    new: String,
}

pub async fn change_password(
    State(state): State<SharedState>,
    user: User,
    Json(req): Json<ChangePasswordReq>,
) -> AppResult<Json<serde_json::Value>> {
    if req.new.len() < 8 {
        return Err(AppError::bad("password must be at least 8 characters"));
    }
    let uid = user.id;
    let hash: String =
        state.db(move |c| c.query_row("SELECT password_hash FROM users WHERE id = ?1", [uid], |r| r.get(0))).await?;
    if !verify_password(req.current, hash).await {
        return Err(AppError::bad("current password is wrong"));
    }
    let new_hash = hash_password(req.new).await?;
    state
        .db(move |c| c.execute("UPDATE users SET password_hash = ?2 WHERE id = ?1", params![uid, new_hash]))
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_tokens(State(state): State<SharedState>, user: User) -> AppResult<Json<serde_json::Value>> {
    let rows = state
        .db(move |c| {
            let mut st = c.prepare("SELECT id, name, created_at, last_used FROM tokens WHERE user_id = ?1 ORDER BY id")?;
            st.query_map([user.id], |r| {
                Ok(json!({ "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?,
                           "createdAt": r.get::<_, i64>(2)?, "lastUsed": r.get::<_, Option<i64>>(3)? }))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    Ok(Json(json!(rows)))
}

#[derive(Deserialize)]
pub struct CreateTokenReq {
    #[serde(default)]
    name: String,
}

pub async fn create_token(
    State(state): State<SharedState>,
    user: User,
    Json(req): Json<CreateTokenReq>,
) -> AppResult<Json<serde_json::Value>> {
    let token = format!("llt_{}", random_token(20));
    let hash = sha256_hex(&token);
    let name = if req.name.trim().is_empty() { "git".to_string() } else { req.name.trim().chars().take(60).collect() };
    let id = state
        .db(move |c| {
            c.execute("INSERT INTO tokens (user_id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)", params![
                user.id,
                name,
                hash,
                now()
            ])?;
            Ok(c.last_insert_rowid())
        })
        .await?;
    Ok(Json(json!({ "id": id, "token": token })))
}

pub async fn delete_token(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<i64>,
) -> AppResult<Json<serde_json::Value>> {
    state.db(move |c| c.execute("DELETE FROM tokens WHERE id = ?1 AND user_id = ?2", params![id, user.id])).await?;
    Ok(Json(json!({ "ok": true })))
}

fn require_admin(user: &User) -> AppResult<()> {
    if user.is_admin { Ok(()) } else { Err(AppError::forbidden()) }
}

pub async fn admin_list_users(State(state): State<SharedState>, user: User) -> AppResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    let rows = state
        .db(|c| {
            let mut st = c.prepare(
                "SELECT u.id, u.email, u.name, u.is_admin, u.created_at,
                        (SELECT COUNT(*) FROM projects p WHERE p.owner_id = u.id)
                 FROM users u ORDER BY u.id",
            )?;
            st.query_map([], |r| {
                Ok(json!({ "id": r.get::<_, i64>(0)?, "email": r.get::<_, String>(1)?, "name": r.get::<_, String>(2)?,
                           "isAdmin": r.get::<_, i64>(3)? != 0, "createdAt": r.get::<_, i64>(4)?,
                           "projects": r.get::<_, i64>(5)? }))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    Ok(Json(json!(rows)))
}

#[derive(Deserialize)]
pub struct AdminCreateReq {
    email: String,
    name: String,
    password: String,
    #[serde(default)]
    is_admin: bool,
}

pub async fn admin_create_user(
    State(state): State<SharedState>,
    user: User,
    Json(req): Json<AdminCreateReq>,
) -> AppResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    let id = insert_user(&state, &req.email, &req.name, &req.password, Some(req.is_admin)).await?;
    Ok(Json(json!({ "id": id })))
}

pub async fn admin_delete_user(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<i64>,
) -> AppResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    if id == user.id {
        return Err(AppError::bad("you cannot delete yourself"));
    }
    let owned: Vec<String> = state
        .db(move |c| {
            let mut st = c.prepare("SELECT id FROM projects WHERE owner_id = ?1")?;
            let v = st.query_map([id], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
            c.execute("DELETE FROM users WHERE id = ?1", [id])?;
            Ok(v)
        })
        .await?;
    for pid in owned {
        state.hub.close_room(&pid);
        let _ = tokio::fs::remove_dir_all(state.cfg.project_dir(&pid)).await;
        let _ = tokio::fs::remove_dir_all(state.cfg.project_build_dir(&pid)).await;
    }
    Ok(Json(json!({ "ok": true })))
}
