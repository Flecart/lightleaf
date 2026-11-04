//! Lightleaf — a lightweight, self-hosted collaborative LaTeX & Typst editor.

mod assets;
mod auth;
mod collab;
mod compile;
mod config;
mod db;
mod error;
mod files;
mod git;
mod overleaf;
mod projects;
mod state;
mod util;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{delete, get, post},
};
use std::sync::Arc;
use tower_http::compression::CompressionLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("lightleaf=info,tower_http=warn")),
        )
        .init();

    let cfg = config::Config::from_env();
    std::fs::create_dir_all(cfg.projects_dir())?;
    std::fs::create_dir_all(cfg.build_dir())?;
    let conn = db::open(&cfg.data_dir.join("lightleaf.db"))?;
    let state = Arc::new(state::AppState::new(cfg.clone(), conn));

    if let Some((email, password)) = cfg.bootstrap_admin.clone() {
        auth::bootstrap_admin(&state, &email, &password).await?;
    }

    // Background flusher: persists dirty collaborative docs to disk and evicts idle ones.
    collab::spawn_flusher(state.clone());

    let api = Router::new()
        .route(
            "/health",
            get(|| async {
                axum::Json(serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
            }),
        )
        .route("/config", get(auth::public_config))
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/me", get(auth::me))
        .route("/me/password", post(auth::change_password))
        .route(
            "/me/tokens",
            get(auth::list_tokens).post(auth::create_token),
        )
        .route("/me/tokens/{id}", delete(auth::delete_token))
        .route(
            "/admin/users",
            get(auth::admin_list_users).post(auth::admin_create_user),
        )
        .route("/admin/users/{id}", delete(auth::admin_delete_user))
        .route("/projects", get(projects::list).post(projects::create))
        .route("/projects/import", post(projects::import_zip))
        .route(
            "/projects/{id}",
            get(projects::get)
                .patch(projects::update)
                .delete(projects::remove),
        )
        .route("/projects/{id}/copy", post(projects::copy))
        .route("/projects/{id}/members", post(projects::add_member))
        .route(
            "/projects/{id}/members/{uid}",
            delete(projects::remove_member),
        )
        .route("/projects/{id}/chat", get(projects::chat_history))
        .route("/projects/{id}/tree", get(files::tree))
        .route(
            "/projects/{id}/file",
            get(files::read).delete(files::remove),
        )
        .route("/projects/{id}/files", post(files::create))
        .route("/projects/{id}/rename", post(files::rename))
        .route("/projects/{id}/upload", post(files::upload))
        .route("/projects/{id}/download", get(files::download_zip))
        .route("/projects/{id}/compile", post(compile::compile))
        .route("/projects/{id}/output/{name}", get(compile::output))
        .route("/projects/{id}/synctex/pdf", get(compile::synctex_from_pdf))
        .route(
            "/projects/{id}/synctex/code",
            get(compile::synctex_from_code),
        )
        .route(
            "/projects/{id}/history",
            get(git::history).post(git::snapshot),
        )
        .route("/projects/{id}/history/{sha}", get(git::show_commit))
        .route(
            "/projects/{id}/history/{sha}/restore",
            post(git::restore_file),
        )
        .route(
            "/projects/{id}/remote",
            get(git::get_remote).post(git::set_remote),
        )
        .route("/projects/{id}/remote/pull", post(git::remote_pull))
        .route("/projects/{id}/remote/push", post(git::remote_push))
        .route("/projects/{id}/ws", get(collab::ws_handler))
        .layer(DefaultBodyLimit::max(cfg.max_upload_bytes));

    // Git smart-HTTP endpoints, compatible with `git clone http://host/git/<project-id>`.
    let git_routes = Router::new()
        .route("/{id}/info/refs", get(git::info_refs))
        .route("/{id}/git-upload-pack", post(git::upload_pack))
        .route("/{id}/git-receive-pack", post(git::receive_pack))
        .layer(DefaultBodyLimit::max(cfg.max_upload_bytes * 4));

    let app = Router::new()
        .nest("/api", api)
        .nest("/git", git_routes)
        .fallback(assets::static_handler)
        .layer(CompressionLayer::new())
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!("Lightleaf listening on http://{}", cfg.bind);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    collab::flush_all(&state).await;
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutting down");
}
