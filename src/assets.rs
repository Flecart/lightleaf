//! The compiled frontend is embedded into the binary, so a deployment is a single file.

use axum::{
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// Content-Security-Policy for the single-page app: only our own scripts, no plugins, and no
/// framing by other sites. Inline styles are needed by CodeMirror.
const APP_CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self' ws: wss:; worker-src 'self' blob:; \
    frame-src 'self'; frame-ancestors 'self'; base-uri 'self'; form-action 'self'";

#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") || path.starts_with("git/") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let (file, name) = match Assets::get(path).filter(|_| !path.is_empty()) {
        Some(f) => (f, path.to_string()),
        None => match Assets::get("index.html") {
            Some(f) => (f, "index.html".to_string()),
            None => return (StatusCode::NOT_FOUND, "frontend not built").into_response(),
        },
    };
    let mime = mime_guess::from_path(&name).first_or_octet_stream();
    let html = name.ends_with(".html");
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut res = (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, cache),
        ],
        file.data,
    )
        .into_response();
    if html {
        res.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            header::HeaderValue::from_static(APP_CSP),
        );
    }
    res
}
