//! lightleaf-reviewer: the AI review service for Lightleaf.
//!
//! A separate process that Lightleaf calls over HTTP. It reads a project over git with a
//! short-lived, project-scoped token, runs a team of reviewer agents on Claude or an OpenAI-compatible model, and pushes
//! the comments back as `\ANG[AI: …]{…}` macros in the LaTeX source. It holds no database and
//! no persistent state: jobs live in memory, checkouts in a temporary directory.
//!
//! API (all but /health need `Authorization: Bearer $LIGHTLEAF_REVIEWER_SECRET`):
//!   GET  /health
//!   POST /v1/jobs                              start a review or clear job
//!   GET  /v1/projects/{project}/latest         the project's most recent job
//!   GET  /v1/projects/{project}/jobs/{job}     one job

mod claude;
mod config;
mod git;
mod jobs;
mod latex;
mod llm;
mod openai;
mod pipeline;
mod prompts;

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use jobs::{Job, JobRequest};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::Semaphore;

struct App {
    cfg: Arc<config::Config>,
    llm: Arc<llm::Llm>,
    prompts: Arc<prompts::Prompts>,
    jobs: Mutex<HashMap<String, Job>>,
    slots: Arc<Semaphore>,
}

type Shared = Arc<App>;

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
    let jobs = app.jobs.lock().unwrap();
    let count = |s: &str| jobs.values().filter(|j| j.status == s).count();
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "llmConfigured": app.cfg.llm_configured(),
        "provider": app.cfg.provider,
        "model": app.cfg.model,
        "running": count("running"),
        "queued": count("queued"),
    }))
}

fn set(app: &App, id: &str, f: impl FnOnce(&mut Job)) {
    if let Some(j) = app.jobs.lock().unwrap().get_mut(id) {
        f(j);
    }
}

/// Forgets finished jobs older than a day, keeping memory bounded.
fn prune(jobs: &mut HashMap<String, Job>) {
    let cutoff = jobs::now().saturating_sub(86_400);
    jobs.retain(|_, j| j.finished_at.is_none_or(|t| t > cutoff));
    if jobs.len() > 1000 {
        let mut finished: Vec<(u64, String)> = jobs
            .values()
            .filter_map(|j| j.finished_at.map(|t| (t, j.id.clone())))
            .collect();
        finished.sort();
        for (_, id) in finished.into_iter().take(jobs.len() - 1000) {
            jobs.remove(&id);
        }
    }
}

async fn create_job(State(app): State<Shared>, Json(req): Json<JobRequest>) -> Response {
    if let Err(e) = req.validate() {
        return err(StatusCode::BAD_REQUEST, e);
    }
    if req.kind == "review" && !app.cfg.llm_configured() {
        return err(StatusCode::CONFLICT, app.cfg.llm_missing_message());
    }
    let job = {
        let mut jobs = app.jobs.lock().unwrap();
        prune(&mut jobs);
        if jobs.values().any(|j| {
            j.project_id == req.project_id && (j.status == "queued" || j.status == "running")
        }) {
            return err(
                StatusCode::CONFLICT,
                "a review job is already running for this project",
            );
        }
        let job = Job {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: req.project_id.clone(),
            kind: req.kind.clone(),
            status: "queued".into(),
            stage: "Waiting for a free slot".into(),
            created_at: jobs::now(),
            finished_at: None,
            error: None,
            result: None,
        };
        jobs.insert(job.id.clone(), job.clone());
        job
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
        set(&app2, &id, |j| {
            j.status = "running".into();
            j.stage = "Starting".into();
        });
        let ctx = Arc::new(pipeline::Ctx {
            llm: app2.llm.clone(),
            cfg: app2.cfg.clone(),
            prompts: app2.prompts.clone(),
            usage: Arc::new(llm::Usage::default()),
        });
        let updater = {
            let (app3, id3) = (app2.clone(), id.clone());
            move |stage: String| set(&app3, &id3, |j| j.stage = stage)
        };
        // A panic inside the job must not take the service down: run it in its own task.
        let handle = tokio::spawn(jobs::run(ctx, req, id.clone(), updater));
        let outcome = match handle.await {
            Ok(r) => r,
            Err(e) => Err(anyhow::anyhow!("internal error: {e}")),
        };
        set(&app2, &id, |j| {
            j.finished_at = Some(jobs::now());
            match outcome {
                Ok(result) => {
                    j.status = "done".into();
                    j.stage = "Finished".into();
                    j.result = Some(result);
                }
                Err(e) => {
                    tracing::warn!("job {} failed: {e:#}", j.id);
                    j.status = "failed".into();
                    j.stage = "Failed".into();
                    j.error = Some(format!("{e:#}"));
                }
            }
        });
    });
    (StatusCode::ACCEPTED, Json(json!(job))).into_response()
}

async fn latest(State(app): State<Shared>, Path(project): Path<String>) -> Json<Value> {
    let jobs = app.jobs.lock().unwrap();
    let job = jobs
        .values()
        .filter(|j| j.project_id == project)
        .max_by_key(|j| j.created_at);
    Json(match job {
        Some(j) => json!(j),
        None => json!({ "status": "none" }),
    })
}

async fn get_job(
    State(app): State<Shared>,
    Path((project, id)): Path<(String, String)>,
) -> Response {
    match app.jobs.lock().unwrap().get(&id) {
        Some(j) if j.project_id == project => Json(json!(j)).into_response(),
        _ => err(StatusCode::NOT_FOUND, "no such job"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("lightleaf_reviewer=info"));
    if std::env::var("LIGHTLEAF_LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json")) {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("lightleaf-reviewer {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let cfg = Arc::new(config::Config::from_env()?);
    if !cfg.llm_configured() {
        tracing::warn!(
            "{}: reviews will be refused (removing comments still works)",
            cfg.llm_missing_message()
        );
    }
    let llm = Arc::new(match cfg.provider.as_str() {
        "openai" => llm::Llm::OpenAi(openai::OpenAi::new(
            cfg.openai_base_url.clone(),
            cfg.openai_api_key.clone().unwrap_or_default(),
            cfg.model.clone(),
            cfg.openai_reasoning_effort.clone(),
            cfg.openai_max_tokens_field.clone(),
        )?),
        _ => llm::Llm::Anthropic(claude::Claude::new(
            cfg.anthropic_base_url.clone(),
            cfg.anthropic_api_key.clone().unwrap_or_default(),
            cfg.model.clone(),
            cfg.effort.clone(),
            cfg.fallbacks,
        )?),
    });
    std::fs::create_dir_all(&cfg.work_dir)?;
    let app = Arc::new(App {
        prompts: Arc::new(prompts::Prompts::load(cfg.prompts_dir.as_deref())),
        slots: Arc::new(Semaphore::new(cfg.max_jobs)),
        jobs: Mutex::new(HashMap::new()),
        llm,
        cfg: cfg.clone(),
    });

    let v1 = Router::new()
        .route("/jobs", post(create_job))
        .route("/projects/{project}/latest", get(latest))
        .route("/projects/{project}/jobs/{id}", get(get_job))
        .layer(middleware::from_fn_with_state(app.clone(), require_secret));
    let router = Router::new()
        .route("/health", get(health))
        .nest("/v1", v1)
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(
        "lightleaf-reviewer listening on http://{} ({} model {})",
        cfg.bind,
        cfg.provider,
        cfg.model
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
