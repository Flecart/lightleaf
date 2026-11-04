use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signup {
    /// Anyone can register.
    Open,
    /// Only admins can create accounts (the very first account is always allowed).
    Closed,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub data_dir: PathBuf,
    pub signup: Signup,
    pub compile_timeout_secs: u64,
    pub max_concurrent_compiles: usize,
    /// Optional command prefix used to sandbox compilers, e.g. `bwrap --unshare-net ...`.
    pub compile_wrapper: Vec<String>,
    pub max_upload_bytes: usize,
    pub secure_cookies: bool,
    pub bootstrap_admin: Option<(String, String)>,
    pub site_name: String,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> Self {
        let data_dir = PathBuf::from(env("LIGHTLEAF_DATA").unwrap_or_else(|| "./data".into()));
        Config {
            bind: env("LIGHTLEAF_BIND").unwrap_or_else(|| "0.0.0.0:8080".into()),
            data_dir,
            signup: match env("LIGHTLEAF_SIGNUP").as_deref() {
                Some("closed") | Some("false") | Some("0") => Signup::Closed,
                _ => Signup::Open,
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
            compile_wrapper: env("LIGHTLEAF_COMPILE_WRAPPER")
                .map(|v| v.split_whitespace().map(String::from).collect())
                .unwrap_or_default(),
            max_upload_bytes: env("LIGHTLEAF_MAX_UPLOAD_MB")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(50)
                * 1024
                * 1024,
            secure_cookies: matches!(
                env("LIGHTLEAF_SECURE_COOKIES").as_deref(),
                Some("1") | Some("true")
            ),
            bootstrap_admin: match (
                env("LIGHTLEAF_ADMIN_EMAIL"),
                env("LIGHTLEAF_ADMIN_PASSWORD"),
            ) {
                (Some(e), Some(p)) => Some((e, p)),
                _ => None,
            },
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
