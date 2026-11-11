use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signup {
    /// Anyone can register.
    Open,
    /// No self-registration: admins create accounts in the admin panel or with `lightleaf user add`.
    Closed,
    /// Self-registration only from the server machine itself (e.g. through an SSH tunnel).
    Local,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub data_dir: PathBuf,
    pub signup: Signup,
    pub compile_timeout_secs: u64,
    pub max_concurrent_compiles: usize,
    /// Compiles one user may have running or queued at once.
    pub compiles_per_user: usize,
    /// Compiles one user may run per minute (coalesced requests do not count).
    pub compiles_per_minute: u32,
    /// Optional command prefix used to sandbox compilers, e.g. `bwrap --unshare-net ...`.
    pub compile_wrapper: Vec<String>,
    pub max_upload_bytes: usize,
    /// Projects one user may own (0 = unlimited).
    pub max_projects_per_user: usize,
    /// Maximum size of a project's files, excluding git history (0 = unlimited).
    pub max_project_bytes: u64,
    /// Size of the SQLite connection pool.
    pub db_connections: usize,
    pub secure_cookies: bool,
    /// Honour `X-Forwarded-For` from a reverse proxy when identifying clients (rate limiting).
    pub trust_proxy: bool,
    pub session_days: i64,
    /// Sessions unused for this many days are logged out.
    pub session_idle_days: i64,
    /// Where the source code of this deployment can be downloaded (AGPL-3.0 §13).
    pub source_url: String,
    pub bootstrap_admin: Option<(String, String)>,
    /// The optional AI review service; `None` disables the feature entirely.
    pub reviewer: Option<crate::reviewer::ReviewerConfig>,
    /// Bearer token that grants access to /metrics from other machines.
    pub metrics_token: Option<String>,
    pub site_name: String,
}

/// `LIGHTLEAF_SECRET_KEY` (64 hex characters), the key that encrypts stored remote tokens.
/// Read on its own instead of into `Config`, which derives Debug and must never hold it.
pub fn secret_key() -> Option<String> {
    env("LIGHTLEAF_SECRET_KEY")
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Reads the optional reviewer integration. Misconfiguration disables the feature with a
/// warning instead of failing startup: the editor must never depend on the reviewer.
fn reviewer_config(bind: &str) -> Option<crate::reviewer::ReviewerConfig> {
    let url = env("LIGHTLEAF_REVIEWER_URL")?;
    if !url.starts_with("http://") {
        eprintln!(
            "warning: LIGHTLEAF_REVIEWER_URL must be an http:// URL on an internal network; AI review disabled"
        );
        return None;
    }
    let Some(secret) = env("LIGHTLEAF_REVIEWER_SECRET").filter(|s| s.len() >= 16) else {
        eprintln!(
            "warning: LIGHTLEAF_REVIEWER_SECRET (16+ characters) is required with LIGHTLEAF_REVIEWER_URL; AI review disabled"
        );
        return None;
    };
    let port = bind.rsplit(':').next().unwrap_or("8080");
    Some(crate::reviewer::ReviewerConfig {
        url: url.trim_end_matches('/').to_string(),
        secret,
        git_base: env("LIGHTLEAF_REVIEWER_GIT_URL")
            .unwrap_or_else(|| format!("http://127.0.0.1:{port}")),
        token_ttl_secs: env("LIGHTLEAF_REVIEWER_TOKEN_TTL")
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(3600),
    })
}

impl Config {
    pub fn from_env() -> Self {
        let data_dir = PathBuf::from(env("LIGHTLEAF_DATA").unwrap_or_else(|| "./data".into()));
        let bind = env("LIGHTLEAF_BIND").unwrap_or_else(|| "0.0.0.0:8080".into());
        Config {
            reviewer: reviewer_config(&bind),
            bind,
            data_dir,
            signup: match env("LIGHTLEAF_SIGNUP")
                .map(|v| v.to_ascii_lowercase())
                .as_deref()
            {
                Some("open") | Some("true") | Some("1") => Signup::Open,
                Some("local") => Signup::Local,
                Some("closed") | Some("false") | Some("0") | None => Signup::Closed,
                Some(other) => {
                    eprintln!("warning: unknown LIGHTLEAF_SIGNUP={other:?}, using \"closed\"");
                    Signup::Closed
                }
            },
            compile_timeout_secs: env("LIGHTLEAF_COMPILE_TIMEOUT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(90),
            max_concurrent_compiles: env("LIGHTLEAF_MAX_COMPILES")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| {
                    std::thread::available_parallelism()
                        .map(|n| n.get().clamp(1, 4))
                        .unwrap_or(2)
                }),
            compiles_per_user: env("LIGHTLEAF_COMPILES_PER_USER")
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(2),
            compiles_per_minute: env("LIGHTLEAF_COMPILES_PER_MINUTE")
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(30),
            compile_wrapper: env("LIGHTLEAF_COMPILE_WRAPPER")
                .map(|v| v.split_whitespace().map(String::from).collect())
                .unwrap_or_default(),
            max_upload_bytes: env("LIGHTLEAF_MAX_UPLOAD_MB")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(50)
                * 1024
                * 1024,
            max_projects_per_user: env("LIGHTLEAF_MAX_PROJECTS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            max_project_bytes: env("LIGHTLEAF_MAX_PROJECT_MB")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
                * 1024
                * 1024,
            db_connections: env("LIGHTLEAF_DB_CONNECTIONS")
                .and_then(|v| v.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(4),
            secure_cookies: matches!(
                env("LIGHTLEAF_SECURE_COOKIES").as_deref(),
                Some("1") | Some("true")
            ),
            trust_proxy: matches!(
                env("LIGHTLEAF_TRUST_PROXY").as_deref(),
                Some("1") | Some("true")
            ),
            session_days: env("LIGHTLEAF_SESSION_DAYS")
                .and_then(|v| v.parse().ok())
                .filter(|d| *d > 0)
                .unwrap_or(30),
            session_idle_days: env("LIGHTLEAF_SESSION_IDLE_DAYS")
                .and_then(|v| v.parse().ok())
                .filter(|d| *d > 0)
                .unwrap_or(14),
            source_url: env("LIGHTLEAF_SOURCE_URL")
                .unwrap_or_else(|| env!("CARGO_PKG_REPOSITORY").to_string()),
            bootstrap_admin: match (
                env("LIGHTLEAF_ADMIN_EMAIL"),
                env("LIGHTLEAF_ADMIN_PASSWORD"),
            ) {
                (Some(e), Some(p)) => Some((e, p)),
                _ => None,
            },
            metrics_token: env("LIGHTLEAF_METRICS_TOKEN"),
            site_name: env("LIGHTLEAF_SITE_NAME").unwrap_or_else(|| "Lightleaf".into()),
        }
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.data_dir.join("projects")
    }
    pub fn build_dir(&self) -> PathBuf {
        self.data_dir.join("build")
    }
    pub fn project_dir(&self, id: &str) -> PathBuf {
        self.projects_dir().join(id)
    }
    pub fn project_build_dir(&self, id: &str) -> PathBuf {
        self.build_dir().join(id)
    }
}
