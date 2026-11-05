//! The HTTP API Lightleaf calls. All routes except `/health` require
//! `Authorization: Bearer $LIGHTLEAF_REVIEWER_SECRET`.
//!
//!   GET  /health
//!   POST /v1/jobs                              start a job (see `jobs::JobRequest`)
//!   GET  /v1/projects/{project}/latest         the project's most recent job
//!   GET  /v1/projects/{project}/jobs/{job}     one job

use crate::{
    config::Config,
    jobs::{self, JobRequest, store::JobStore},
    llm::{Llm, Usage},
    prompts::Prompts,
    review::Ctx,
};
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Semaphore;

pub struct App {
    pub cfg: Arc<Config>,
    pub llm: Arc<Llm>,
    pub prompts: Arc<Prompts>,
    pub jobs: JobStore,
    /// Limits how many jobs run at once (`LIGHTLEAF_REVIEW_MAX_JOBS`).
    pub slots: Arc<Semaphore>,
}

type Shared = Arc<App>;

pub fn router(app: Shared) -> Router {
    let v1 = Router::new()
        .route("/jobs", post(create_job))
        .route("/projects/{project}/latest", get(latest))
        .route("/projects/{project}/jobs/{id}", get(get_job))
        .layer(middleware::from_fn_with_state(app.clone(), require_secret));
    Router::new()
        .route("/health", get(health))
        .nest("/v1", v1)
        .with_state(app)
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_secret(State(app): State<Shared>, req: Request, next: Next) -> Response {
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| constant_time_eq(t.as_bytes(), app.cfg.secret.as_bytes()));
    if !ok {
        return err(StatusCode::UNAUTHORIZED, "invalid reviewer secret");
    }
    next.run(req).await
}

async fn health(State(app): State<Shared>) -> Json<Value> {
    let (running, queued) = app.jobs.counts();
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "llmConfigured": app.cfg.llm_configured(),
        "provider": app.llm.provider(),
        "model": app.llm.model(),
        "running": running,
        "queued": queued,
    }))
}

async fn create_job(State(app): State<Shared>, Json(req): Json<JobRequest>) -> Response {
    let kind = match req.validate() {
        Ok(k) => k,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    if kind.needs_llm() && !app.cfg.llm_configured() {
        return err(StatusCode::CONFLICT, app.cfg.llm_missing_message());
    }
    let job = match app.jobs.create(&req.project_id, &req.kind) {
        Ok(j) => j,
        Err(e) => return err(StatusCode::CONFLICT, e),
    };
    tracing::info!(
        "job {} ({}) for project {} \"{}\" requested by {}",
        job.id,
        job.kind,
        req.project_id,
        req.project_name,
        req.requested_by.email
    );
    let (app2, id) = (app.clone(), job.id.clone());
    tokio::spawn(async move {
        let _permit = app2.slots.clone().acquire_owned().await;
        app2.jobs.start(&id);
        let ctx = Arc::new(Ctx {
            llm: app2.llm.clone(),
            cfg: app2.cfg.clone(),
            prompts: app2.prompts.clone(),
            usage: Arc::new(Usage::default()),
        });
        let progress = {
            let (app3, id3) = (app2.clone(), id.clone());
            Box::new(move |stage: String| app3.jobs.set_stage(&id3, stage))
        };
        // Run in its own task so that even a panic only fails this job.
        let outcome = match tokio::spawn(jobs::run(ctx, kind, req, id.clone(), progress)).await {
            Ok(r) => r,
            Err(e) => Err(anyhow::anyhow!("internal error: {e}")),
        };
        app2.jobs.finish(&id, outcome);
    });
    (StatusCode::ACCEPTED, Json(json!(job))).into_response()
}

async fn latest(State(app): State<Shared>, Path(project): Path<String>) -> Json<Value> {
    Json(
        app.jobs
            .latest(&project)
            .map(|j| json!(j))
            .unwrap_or_else(|| json!({ "status": "none" })),
    )
}

async fn get_job(
    State(app): State<Shared>,
    Path((project, id)): Path<(String, String)>,
) -> Response {
    match app.jobs.get(&project, &id) {
        Some(j) => Json(json!(j)).into_response(),
        None => err(StatusCode::NOT_FOUND, "no such job"),
    }
}
