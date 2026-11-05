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
    extract::{ConnectInfo, FromRequestParts, Path, State},
    http::{HeaderMap, HeaderValue, header, request::Parts},
    response::{IntoResponse, Response},
};
use base64::Engine;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{net::SocketAddr, sync::OnceLock};

const SESSION_COOKIE: &str = "ll_session";
/// How often a session's `last_seen` is written back (avoids a DB write per request).
const TOUCH_INTERVAL: i64 = 300;
pub const MIN_PASSWORD: usize = 8;
const MAX_PASSWORD: usize = 1024;

#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
}

fn row_to_user(r: &rusqlite::Row) -> rusqlite::Result<User> {
    Ok(User {
        id: r.get(0)?,
        email: r.get(1)?,
        name: r.get(2)?,
        is_admin: r.get::<_, i64>(3)? != 0,
    })
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| {
            v.split(';').find_map(|kv| {
                let (k, val) = kv.trim().split_once('=')?;
                (k == name).then(|| val.to_string())
            })
        })
}

pub async fn user_from_session(state: &AppState, headers: &HeaderMap) -> AppResult<Option<User>> {
    let Some(token) = cookie_value(headers, SESSION_COOKIE) else {
        return Ok(None);
    };
    let hash = sha256_hex(&token);
    let idle = state.cfg.session_idle_days * 86400;
    state
        .db(move |c| {
            let t = now();
            let found = c
                .prepare_cached(
                    "SELECT u.id, u.email, u.name, u.is_admin, s.last_seen FROM sessions s JOIN users u ON u.id = s.user_id
                     WHERE s.token_hash = ?1 AND s.expires_at > ?2 AND s.last_seen > ?3",
                )?
                .query_row(params![hash, t, t - idle], |r| {
                    Ok((row_to_user(r)?, r.get::<_, i64>(4)?))
                })
                .optional()?;
            let Some((user, last_seen)) = found else {
                return Ok(None);
            };
            if t - last_seen > TOUCH_INTERVAL {
                c.execute("UPDATE sessions SET last_seen = ?2 WHERE token_hash = ?1", params![hash, t])?;
            }
            Ok(Some(user))
        })
        .await
}

/// Authenticates git clients via HTTP Basic auth: any username + a personal access token
/// as the password (like Overleaf's git bridge).
pub async fn user_from_basic(state: &AppState, headers: &HeaderMap) -> AppResult<Option<User>> {
    let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return Ok(None);
    };
    let Some(b64) = v.strip_prefix("Basic ") else {
        return Ok(None);
    };
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
        return Ok(None);
    };
    let raw = String::from_utf8_lossy(&raw).to_string();
    let Some((_user, secret)) = raw.split_once(':') else {
        return Ok(None);
    };
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
    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        user_from_session(state, &parts.headers)
            .await?
            .ok_or_else(AppError::unauthorized)
    }
}

async fn hash_password(state: &AppState, pw: String) -> AppResult<String> {
    let _slot = state
        .hash_slots
        .acquire()
        .await
        .map_err(anyhow::Error::from)?;
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

/// A real Argon2 hash of a random password, verified against when an account does not exist so
/// that unknown and known emails take the same time.
fn dummy_hash() -> String {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| {
        Argon2::default()
            .hash_password(random_token(16).as_bytes())
            .map(|h| h.to_string())
            .unwrap_or_default()
    })
    .clone()
}

async fn verify_password(state: &AppState, pw: String, hash: Option<String>) -> bool {
    let Ok(_slot) = state.hash_slots.acquire().await else {
        return false;
    };
    tokio::task::spawn_blocking(move || {
        let real = hash.is_some();
        let hash = hash.unwrap_or_else(dummy_hash);
        let ok = PasswordHash::new(&hash)
            .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
            .unwrap_or(false);
        real && ok
    })
    .await
    .unwrap_or(false)
}

pub fn validate_password(password: &str) -> AppResult<()> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(AppError::bad(format!(
            "password must be at least {MIN_PASSWORD} characters"
        )));
    }
    if password.len() > MAX_PASSWORD {
        return Err(AppError::bad("password is too long"));
    }
    Ok(())
}

fn validate_new_account(email: &str, name: &str, password: &str) -> AppResult<()> {
    let email = email.trim();
    let valid_email = email.len() <= 200
        && !email.chars().any(char::is_whitespace)
        && email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty());
    if !valid_email {
        return Err(AppError::bad("invalid email address"));
    }
    if name.trim().is_empty() || name.len() > 100 {
        return Err(AppError::bad("invalid name"));
    }
    validate_password(password)
}

pub async fn insert_user(
    state: &AppState,
    email: &str,
    name: &str,
    password: &str,
    admin: Option<bool>,
) -> AppResult<i64> {
    validate_new_account(email, name, password)?;
    let hash = hash_password(state, password.to_string()).await?;
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
        .db(move |c| {
            c.query_row("SELECT COUNT(*) FROM users WHERE email = ?1", [e], |r| {
                r.get::<_, i64>(0)
            })
        })
        .await
        .map_err(|e| anyhow::anyhow!(e.1))?
        > 0;
    if !exists {
        insert_user(state, email, "Admin", password, Some(true))
            .await
            .map_err(|e| anyhow::anyhow!(e.1))?;
        tracing::info!("created bootstrap admin {email}");
    }
    Ok(())
}

/// Sets a user's password. All of their sessions are revoked except `keep` (a session token hash).
pub async fn set_password(
    state: &AppState,
    uid: i64,
    password: String,
    keep: Option<String>,
) -> AppResult<()> {
    validate_password(&password)?;
    let hash = hash_password(state, password).await?;
    state
        .db(move |c| {
            c.execute(
                "UPDATE users SET password_hash = ?2 WHERE id = ?1",
                params![uid, hash],
            )?;
            c.execute(
                "DELETE FROM sessions WHERE user_id = ?1 AND token_hash IS NOT ?2",
                params![uid, keep],
            )
        })
        .await?;
    Ok(())
}

fn cookie_header(state: &AppState, value: &str, max_age: i64) -> AppResult<HeaderValue> {
    let secure = if state.cfg.secure_cookies {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .map_err(|e| anyhow::Error::from(e).into())
}

fn user_agent(headers: &HeaderMap) -> String {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

async fn start_session(
    state: &AppState,
    user_id: i64,
    ip: String,
    ua: String,
) -> AppResult<Response> {
    let token = random_token(32);
    let hash = sha256_hex(&token);
    let max_age = state.cfg.session_days * 86400;
    let idle = state.cfg.session_idle_days * 86400;
    state
        .db(move |c| {
            let t = now();
            c.execute(
                "DELETE FROM sessions WHERE expires_at < ?1 OR last_seen < ?2",
                params![t, t - idle],
            )?;
            c.execute(
                "INSERT INTO sessions (token_hash, user_id, expires_at, created_at, last_seen, ip, user_agent)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
                params![hash, user_id, t + max_age, t, ip, ua],
            )
        })
        .await?;
    let mut res = Json(json!({ "ok": true })).into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, cookie_header(state, &token, max_age)?);
    Ok(res)
}

fn too_many(retry_after: u64) -> AppError {
    AppError(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        format!(
            "too many attempts — try again in {} min",
            retry_after.div_ceil(60)
        ),
    )
}

/// Rejects the request if this client IP has made too many failed attempts.
fn check_ip(state: &AppState, ip: &str) -> AppResult<()> {
    if let Some(wait) = state.limits.auth_per_ip.blocked(ip) {
        tracing::warn!("rate limited authentication attempts from {ip}");
        return Err(too_many(wait));
    }
    Ok(())
}

/// Whether a web registration is allowed right now, and if not, why.
fn signup_policy(state: &AppState, users: i64, local: bool) -> Result<(), &'static str> {
    if users == 0 {
        // The first account becomes the administrator: only allow claiming it from the server.
        return if local {
            Ok(())
        } else {
            Err(
                "No administrator exists yet. Create one on the server with `lightleaf user add --admin <email>`, \
                 or set LIGHTLEAF_ADMIN_EMAIL and LIGHTLEAF_ADMIN_PASSWORD.",
            )
        };
    }
    match state.cfg.signup {
        Signup::Open => Ok(()),
        Signup::Local if local => Ok(()),
        Signup::Local => Err(
            "Registration is only possible from the server itself. Ask an administrator for an account.",
        ),
        Signup::Closed => Err("Registration is closed. Ask an administrator for an account."),
    }
}

async fn user_count(state: &AppState) -> AppResult<i64> {
    state
        .db(|c| c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)))
        .await
}

pub async fn public_config(
    State(state): State<SharedState>,
    ci: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> AppResult<Json<serde_json::Value>> {
    let users = user_count(&state).await?;
    let local = crate::security::is_local_request(Some(crate::security::peer(&ci)), &headers);
    let policy = signup_policy(&state, users, local);
    let compilers = crate::compile::available_compilers();
    Ok(Json(json!({
        "siteName": state.cfg.site_name,
        "signup": policy.is_ok(),
        "signupMessage": policy.err(),
        "firstUser": users == 0,
        "compilers": compilers,
        "sourceUrl": state.cfg.source_url,
        "version": env!("CARGO_PKG_VERSION"),
    })))
}

#[derive(Deserialize)]
pub struct RegisterReq {
    email: String,
    name: String,
    password: String,
}

pub async fn register(
    State(state): State<SharedState>,
    ci: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<RegisterReq>,
) -> AppResult<Response> {
    let peer = crate::security::peer(&ci);
    let ip = crate::security::client_ip(state.cfg.trust_proxy, Some(peer), &headers);
    check_ip(&state, &ip)?;
    // Every registration attempt counts, so one client cannot mass-create accounts.
    state.limits.auth_per_ip.hit(&ip);
    let local = crate::security::is_local_request(Some(peer), &headers);
    let users = user_count(&state).await?;
    if let Err(msg) = signup_policy(&state, users, local) {
        return Err(AppError(axum::http::StatusCode::FORBIDDEN, msg.into()));
    }
    let id = insert_user(&state, &req.email, &req.name, &req.password, None).await?;
    tracing::info!("new account {} registered from {ip}", req.email.trim());
    start_session(&state, id, ip, user_agent(&headers)).await
}

#[derive(Deserialize)]
pub struct LoginReq {
    email: String,
    password: String,
}

pub async fn login(
    State(state): State<SharedState>,
    ci: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> AppResult<Response> {
    let ip = crate::security::client_ip(
        state.cfg.trust_proxy,
        Some(crate::security::peer(&ci)),
        &headers,
    );
    check_ip(&state, &ip)?;
    let email = req.email.trim().to_lowercase();
    if let Some(wait) = state.limits.failed_per_account.blocked(&email) {
        return Err(too_many(wait));
    }
    let e = email.clone();
    let row: Option<(i64, String)> = state
        .db(move |c| {
            c.query_row(
                "SELECT id, password_hash FROM users WHERE email = ?1",
                [e],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
        })
        .await?;
    let (id, hash) = match row {
        Some((id, h)) => (Some(id), Some(h)),
        None => (None, None),
    };
    if verify_password(&state, req.password, hash).await
        && let Some(id) = id
    {
        state.limits.failed_per_account.clear(&email);
        return start_session(&state, id, ip, user_agent(&headers)).await;
    }
    state.limits.failed_per_account.hit(&email);
    state.limits.auth_per_ip.hit(&ip);
    tracing::info!("failed login for {email} from {ip}");
    Err(AppError(
        axum::http::StatusCode::UNAUTHORIZED,
        "invalid email or password".into(),
    ))
}

pub async fn logout(State(state): State<SharedState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(token) = cookie_value(&headers, SESSION_COOKIE) {
        let hash = sha256_hex(&token);
        state
            .db(move |c| c.execute("DELETE FROM sessions WHERE token_hash = ?1", [hash]))
            .await?;
    }
    let mut res = Json(json!({ "ok": true })).into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, cookie_header(&state, "", 0)?);
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
    headers: HeaderMap,
    Json(req): Json<ChangePasswordReq>,
) -> AppResult<Json<serde_json::Value>> {
    validate_password(&req.new)?;
    let uid = user.id;
    let hash: String = state
        .db(move |c| {
            c.query_row(
                "SELECT password_hash FROM users WHERE id = ?1",
                [uid],
                |r| r.get(0),
            )
        })
        .await?;
    if !verify_password(&state, req.current, Some(hash)).await {
        return Err(AppError::bad("current password is wrong"));
    }
    let current = cookie_value(&headers, SESSION_COOKIE).map(|t| sha256_hex(&t));
    set_password(&state, uid, req.new, current).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Lists the user's active sessions (browsers that are logged in).
pub async fn list_sessions(
    State(state): State<SharedState>,
    user: User,
    headers: HeaderMap,
) -> AppResult<Json<serde_json::Value>> {
    let current = cookie_value(&headers, SESSION_COOKIE).map(|t| sha256_hex(&t));
    let idle = state.cfg.session_idle_days * 86400;
    let rows = state
        .db(move |c| {
            let t = now();
            let mut st = c.prepare(
                "SELECT rowid, created_at, last_seen, ip, user_agent, token_hash = ?2 FROM sessions
                 WHERE user_id = ?1 AND expires_at > ?3 AND last_seen > ?4 ORDER BY last_seen DESC",
            )?;
            st.query_map(params![user.id, current, t, t - idle], |r| {
                Ok(json!({ "id": r.get::<_, i64>(0)?, "createdAt": r.get::<_, i64>(1)?, "lastSeen": r.get::<_, i64>(2)?,
                           "ip": r.get::<_, String>(3)?, "userAgent": r.get::<_, String>(4)?,
                           "current": r.get::<_, Option<bool>>(5)?.unwrap_or(false) }))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .await?;
    Ok(Json(json!(rows)))
}

pub async fn delete_session(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<i64>,
) -> AppResult<Json<serde_json::Value>> {
    state
        .db(move |c| {
            c.execute(
                "DELETE FROM sessions WHERE rowid = ?1 AND user_id = ?2",
                params![id, user.id],
            )
        })
        .await?;
    Ok(Json(json!({ "ok": true })))
}

/// Logs out every other browser.
pub async fn delete_other_sessions(
    State(state): State<SharedState>,
    user: User,
    headers: HeaderMap,
) -> AppResult<Json<serde_json::Value>> {
    let current = cookie_value(&headers, SESSION_COOKIE).map(|t| sha256_hex(&t));
    let n = state
        .db(move |c| {
            c.execute(
                "DELETE FROM sessions WHERE user_id = ?1 AND token_hash IS NOT ?2",
                params![user.id, current],
            )
        })
        .await?;
    Ok(Json(json!({ "ok": true, "revoked": n })))
}

pub async fn list_tokens(
    State(state): State<SharedState>,
    user: User,
) -> AppResult<Json<serde_json::Value>> {
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
    let name = if req.name.trim().is_empty() {
        "git".to_string()
    } else {
        req.name.trim().chars().take(60).collect()
    };
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
    state
        .db(move |c| {
            c.execute(
                "DELETE FROM tokens WHERE id = ?1 AND user_id = ?2",
                params![id, user.id],
            )
        })
        .await?;
    Ok(Json(json!({ "ok": true })))
}

fn require_admin(user: &User) -> AppResult<()> {
    if user.is_admin {
        Ok(())
    } else {
        Err(AppError::forbidden())
    }
}

pub async fn admin_list_users(
    State(state): State<SharedState>,
    user: User,
) -> AppResult<Json<serde_json::Value>> {
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
    let id = insert_user(
        &state,
        &req.email,
        &req.name,
        &req.password,
        Some(req.is_admin),
    )
    .await?;
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
            let v = st
                .query_map([id], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
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

/// Sets a new random password for a user and logs them out everywhere. Returns the password once.
pub async fn admin_reset_password(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<i64>,
) -> AppResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    if id == user.id {
        return Err(AppError::bad(
            "change your own password in account settings",
        ));
    }
    let password = random_password();
    set_password(&state, id, password.clone(), None).await?;
    tracing::info!("admin {} reset the password of user {id}", user.email);
    Ok(Json(json!({ "password": password })))
}

#[derive(Deserialize)]
pub struct AdminUpdateReq {
    is_admin: bool,
}

pub async fn admin_update_user(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<i64>,
    Json(req): Json<AdminUpdateReq>,
) -> AppResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    if id == user.id && !req.is_admin {
        return Err(AppError::bad("you cannot remove your own admin rights"));
    }
    let n = state
        .db(move |c| {
            c.execute(
                "UPDATE users SET is_admin = ?2 WHERE id = ?1",
                params![id, req.is_admin as i64],
            )
        })
        .await?;
    if n == 0 {
        return Err(AppError::not_found());
    }
    Ok(Json(json!({ "ok": true })))
}

/// A random 20-character password from an unambiguous alphabet (~115 bits).
pub fn random_password() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..20)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_and_password_validation() {
        assert!(validate_new_account("a@b.c", "A", "longenough").is_ok());
        assert!(validate_new_account("@b.c", "A", "longenough").is_err());
        assert!(validate_new_account("a@", "A", "longenough").is_err());
        assert!(validate_new_account("a b@c.d", "A", "longenough").is_err());
        assert!(validate_new_account("a@b.c", " ", "longenough").is_err());
        assert!(validate_new_account("a@b.c", "A", "short").is_err());
        assert!(validate_password(&"x".repeat(2000)).is_err());
    }

    #[test]
    fn random_passwords_are_valid() {
        let p = random_password();
        assert_eq!(p.len(), 20);
        assert!(validate_password(&p).is_ok());
        assert_ne!(p, random_password());
    }
}
