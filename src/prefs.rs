//! Per-user editor preferences that follow the account across browsers.
//!
//! The only one today is the Latex Suite configuration (snippets, snippet variables and the
//! plain settings, in the shape of Obsidian's `data.json`). The server stores it as an opaque
//! JSON object: only the browser of the same user interprets it, and it is never executed.

use crate::{
    auth::User,
    error::{AppError, AppResult},
    state::SharedState,
    util::now,
};
use axum::{Json, extract::State};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

/// Upper bound for one stored preference (serialized JSON).
const MAX_PREF_BYTES: usize = 256 * 1024;

const LATEX_SUITE: &str = "latex_suite";

/// Check a preference value and serialize it for storage.
fn encode(value: &Value) -> AppResult<String> {
    if !value.is_object() {
        return Err(AppError::bad("settings must be a JSON object"));
    }
    let text = value.to_string();
    if text.len() > MAX_PREF_BYTES {
        return Err(AppError::bad(format!(
            "settings are too large (max {} KiB)",
            MAX_PREF_BYTES / 1024
        )));
    }
    Ok(text)
}

/// `GET /api/me/latex-suite`: `{ "settings": {…} }`, or `null` when the user keeps the defaults.
pub async fn get_latex_suite(
    State(state): State<SharedState>,
    user: User,
) -> AppResult<Json<Value>> {
    let stored: Option<String> = state
        .db(move |c| {
            c.query_row(
                "SELECT value FROM user_prefs WHERE user_id = ?1 AND key = ?2",
                params![user.id, LATEX_SUITE],
                |r| r.get(0),
            )
            .optional()
        })
        .await?;
    let settings = match stored {
        Some(s) => serde_json::from_str(&s)?,
        None => Value::Null,
    };
    Ok(Json(json!({ "settings": settings })))
}

#[derive(Deserialize)]
pub struct PutLatexSuite {
    settings: Value,
}

/// `PUT /api/me/latex-suite`: store the settings object; `null` goes back to the defaults.
pub async fn put_latex_suite(
    State(state): State<SharedState>,
    user: User,
    Json(req): Json<PutLatexSuite>,
) -> AppResult<Json<Value>> {
    let text = if req.settings.is_null() {
        None
    } else {
        Some(encode(&req.settings)?)
    };
    state
        .db(move |c| match text {
            Some(t) => c.execute(
                "INSERT INTO user_prefs (user_id, key, value, updated_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (user_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![user.id, LATEX_SUITE, t, now()],
            ),
            None => c.execute(
                "DELETE FROM user_prefs WHERE user_id = ?1 AND key = ?2",
                params![user.id, LATEX_SUITE],
            ),
        })
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_objects_within_the_limit() {
        assert!(encode(&json!({ "snippets": "[]", "snippetsEnabled": true })).is_ok());
        assert!(encode(&json!([1, 2])).is_err());
        assert!(encode(&json!("x")).is_err());
        assert!(encode(&json!({ "snippets": "x".repeat(MAX_PREF_BYTES) })).is_err());
    }
}
