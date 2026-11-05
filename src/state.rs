use crate::{
    collab::Hub,
    config::Config,
    error::{AppError, AppResult},
};
use rusqlite::{Connection, OptionalExtension};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Semaphore;

pub type SharedState = Arc<AppState>;

pub struct AppState {
    pub cfg: Config,
    /// A small pool of SQLite connections. With WAL, readers run in parallel and never wait for
    /// a writer; writers are serialised by SQLite itself (busy_timeout).
    db: Arc<Vec<Mutex<Connection>>>,
    next_db: AtomicUsize,
    pub hub: Hub,
    pub compile_slots: Semaphore,
    /// Bounds concurrent Argon2 computations so a login flood cannot exhaust memory.
    pub hash_slots: Semaphore,
    pub limits: crate::security::Limits,
    /// Last compile result per (project, compiler, main file), with the time it started.
    recent_compiles: Mutex<HashMap<String, (std::time::Instant, serde_json::Value)>>,
    /// One async lock per project, serialising disk-level operations (flush, git, compile sync).
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Role {
    Viewer = 1,
    Editor = 2,
    Owner = 3,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Owner => "owner",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "editor" => Some(Role::Editor),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }
    pub fn can_write(self) -> bool {
        self >= Role::Editor
    }
}

impl AppState {
    pub fn new(cfg: Config, conns: Vec<Connection>) -> Self {
        assert!(
            !conns.is_empty(),
            "at least one database connection is required"
        );
        AppState {
            compile_slots: Semaphore::new(cfg.max_concurrent_compiles),
            hash_slots: Semaphore::new(4),
            limits: crate::security::Limits::default(),
            recent_compiles: Mutex::new(HashMap::new()),
            cfg,
            db: Arc::new(conns.into_iter().map(Mutex::new).collect()),
            next_db: AtomicUsize::new(0),
            hub: Hub::default(),
            locks: Mutex::new(HashMap::new()),
        }
    }

    /// Runs a closure against the SQLite connection on the blocking pool.
    pub async fn db<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let db = self.db.clone();
        let start = self.next_db.fetch_add(1, Ordering::Relaxed);
        let res = tokio::task::spawn_blocking(move || {
            // Take the first idle connection, starting at a rotating offset; wait only if all
            // are busy.
            let n = db.len();
            let conn = (0..n)
                .find_map(|i| db[(start + i) % n].try_lock().ok())
                .unwrap_or_else(|| db[start % n].lock().unwrap_or_else(|p| p.into_inner()));
            f(&conn)
        })
        .await
        .map_err(anyhow::Error::from)?;
        Ok(res?)
    }

    /// A compile result for `key` that started at or after `since`, if any.
    pub fn recent_compile(
        &self,
        key: &str,
        since: std::time::Instant,
    ) -> Option<serde_json::Value> {
        let m = self
            .recent_compiles
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        m.get(key)
            .filter(|(t, _)| *t >= since)
            .map(|(_, v)| v.clone())
    }

    pub fn remember_compile(
        &self,
        key: String,
        started: std::time::Instant,
        result: serde_json::Value,
    ) {
        let mut m = self
            .recent_compiles
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // Only the coalescing window matters, so old entries can go.
        if m.len() > 1000 {
            m.retain(|_, (t, _)| t.elapsed() < std::time::Duration::from_secs(600));
        }
        m.insert(key, (started, result));
    }

    /// Drops per-project locks nobody holds or waits for, so the map does not grow with every
    /// project ever touched.
    pub fn prune_locks(&self) {
        let mut locks = self.locks.lock().unwrap_or_else(|p| p.into_inner());
        locks.retain(|_, l| Arc::strong_count(l) > 1);
    }

    pub fn project_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap();
        locks.entry(id.to_string()).or_default().clone()
    }

    /// Returns the role of `user_id` on project `id`, or an error if no access.
    pub async fn role(&self, project_id: &str, user_id: i64) -> AppResult<Role> {
        let pid = project_id.to_string();
        let r: Option<String> = self
            .db(move |c| {
                c.prepare_cached(
                    "SELECT CASE WHEN p.owner_id = ?2 THEN 'owner' ELSE m.role END
                     FROM projects p LEFT JOIN members m ON m.project_id = p.id AND m.user_id = ?2
                     WHERE p.id = ?1",
                )?
                .query_row(rusqlite::params![pid, user_id], |r| {
                    r.get::<_, Option<String>>(0)
                })
                .optional()
                .map(|o| o.flatten())
            })
            .await?;
        match r.as_deref().and_then(Role::parse) {
            Some(role) => Ok(role),
            None => Err(AppError::not_found()),
        }
    }

    pub async fn require(&self, project_id: &str, user_id: i64, min: Role) -> AppResult<Role> {
        let role = self.role(project_id, user_id).await?;
        if role < min {
            return Err(AppError::forbidden());
        }
        Ok(role)
    }

    pub async fn touch_project(&self, project_id: &str) {
        let pid = project_id.to_string();
        let _ = self
            .db(move |c| {
                c.execute(
                    "UPDATE projects SET updated_at = ?2 WHERE id = ?1",
                    rusqlite::params![pid, crate::util::now()],
                )
            })
            .await;
    }
}
