use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[derive(Debug)]
pub struct AppError(pub StatusCode, pub String);

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn bad(msg: impl Into<String>) -> Self {
        AppError(StatusCode::BAD_REQUEST, msg.into())
    }
    pub fn unauthorized() -> Self {
        AppError(StatusCode::UNAUTHORIZED, "not logged in".into())
    }
    pub fn forbidden() -> Self {
        AppError(StatusCode::FORBIDDEN, "permission denied".into())
    }
    pub fn not_found() -> Self {
        AppError(StatusCode::NOT_FOUND, "not found".into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        AppError(StatusCode::CONFLICT, msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if self.0.is_server_error() {
            tracing::error!("{}", self.1);
        }
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        let e: anyhow::Error = e.into();
        AppError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}
