//! Lightleaf — a lightweight, self-hosted collaborative LaTeX & Typst editor.

mod assets;
mod auth;
mod cli;
mod collab;
mod compile;
mod config;
mod db;
mod error;
mod files;
mod git;
mod metrics;
mod overleaf;
mod projects;
mod remote;
mod reviewer;
mod secrets;
mod security;
mod state;
mod util;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, HeaderValue, header},
    middleware,
    routing::{delete, get, post},
};
use std::{net::SocketAddr, sync::Arc};
use tower_http::{
    compression::{
        CompressionLayer,
        predicate::{NotForContentType, Predicate, SizeAbove},
    },
    set_header::SetResponseHeaderLayer,
};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str);
    if matches!(command, Some("help" | "--help" | "-h")) {
        print!("{}", cli::USAGE);
        return Ok(());
    }
    if matches!(command, Some("--version" | "-V")) {
        println!("lightleaf {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("lightleaf=info,tower_http=warn"));
    // LIGHTLEAF_LOG_FORMAT=json emits one JSON object per line, for log aggregators.
    if std::env::var("LIGHTLEAF_LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json")) {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    let cfg = config::Config::from_env();
    std::fs::create_dir_all(cfg.projects_dir())?;
    std::fs::create_dir_all(cfg.build_dir())?;
    let db_path = cfg.data_dir.join("lightleaf.db");
    // The CLI needs one connection; the server a few, so reads run in parallel.
    let pool = if command.is_some_and(|c| c != "serve") {
        1
    } else {
        cfg.db_connections
    };
    let conns = (0..pool)
        .map(|_| db::open(&db_path))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let state = Arc::new(state::AppState::new(cfg.clone(), conns));

    if let Some(c) = command
        && c != "serve"
    {
        return cli::run(&state, &args).await;
    }

    // Loaded only when serving, so a CLI run as another user never creates a key file the server
    // cannot read.
    let secrets = secrets::SecretBox::load(&cfg.data_dir, config::secret_key().as_deref())?;
    let _ = state.secrets.set(secrets);

    if let Some((email, password)) = cfg.bootstrap_admin.clone() {
        auth::bootstrap_admin(&state, &email, &password).await?;
    }

    assets::warm();

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
            "/me/sessions",
            get(auth::list_sessions).delete(auth::delete_other_sessions),
        )
        .route("/me/sessions/{id}", delete(auth::delete_session))
        .route(
            "/admin/users",
            get(auth::admin_list_users).post(auth::admin_create_user),
        )
        .route(
            "/admin/users/{id}",
            delete(auth::admin_delete_user).patch(auth::admin_update_user),
        )
        .route(
            "/admin/users/{id}/password",
            post(auth::admin_reset_password),
        )
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
            get(remote::get_remote).post(remote::set_remote),
        )
        .route("/projects/{id}/remote/pull", post(remote::remote_pull))
        .route("/projects/{id}/remote/push", post(remote::remote_push))
        .route("/projects/{id}/remote/sync", post(remote::remote_sync))
        .route("/projects/{id}/ws", get(collab::ws_handler))
        // Optional AI review (src/reviewer.rs): 404 when not configured, 503 when unreachable.
        .route(
            "/projects/{id}/review",
            get(reviewer::latest).post(reviewer::start),
        )
        .route("/projects/{id}/review/clear", post(reviewer::clear))
        .route(
            "/projects/{id}/review/references",
            post(reviewer::references),
        )
        .route("/projects/{id}/review/jobs/{job}", get(reviewer::status))
        .route("/reviewer/health", get(reviewer::health))
        .layer(DefaultBodyLimit::max(cfg.max_upload_bytes))
        .layer(middleware::from_fn(security::csrf_guard));

    // Git smart-HTTP endpoints, compatible with `git clone http://host/git/<project-id>`.
    let git_routes = Router::new()
        .route("/{id}/info/refs", get(git::info_refs))
        .route("/{id}/git-upload-pack", post(git::upload_pack))
        .route("/{id}/git-receive-pack", post(git::receive_pack))
        .layer(DefaultBodyLimit::max(cfg.max_upload_bytes * 4));

    let app = Router::new()
        .nest("/api", api)
        .nest("/git", git_routes)
        .route("/metrics", get(metrics::handler))
        .fallback(assets::static_handler)
        // Only compress what benefits: PDFs, zips, images and git packs are already compressed,
        // and recompressing them on every download would be the biggest CPU cost after TeX.
        .layer(
            CompressionLayer::new().compress_when(
                SizeAbove::new(1024)
                    .and(NotForContentType::IMAGES)
                    .and(NotForContentType::const_new("application/pdf"))
                    .and(NotForContentType::const_new("application/zip"))
                    .and(NotForContentType::const_new("application/octet-stream"))
                    .and(NotForContentType::const_new("application/x-git"))
                    .and(NotForContentType::const_new("font/")),
            ),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::hsts,
        ))
        .layer(header_layer(header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .layer(header_layer(header::X_FRAME_OPTIONS, "SAMEORIGIN"))
        .layer(header_layer(header::REFERRER_POLICY, "same-origin"))
        .with_state(state.clone());

    if let Some(r) = &cfg.reviewer {
        tracing::info!(
            "AI review enabled (reviewer at {}, git via {})",
            r.url,
            r.git_base
        );
    }
    if cfg.signup == config::Signup::Open {
        tracing::warn!(
            "LIGHTLEAF_SIGNUP=open: anyone who can reach this server can create an account"
        );
    }

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!("Lightleaf listening on http://{}", cfg.bind);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    collab::flush_all(&state).await;
    Ok(())
}

fn header_layer(name: HeaderName, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(name, HeaderValue::from_static(value))
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
