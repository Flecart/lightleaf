//! Compilation of LaTeX (folio, or pdflatex through latexmk or a manual rerun pipeline) and
//! Typst projects, plus SyncTeX lookups.
//!
//! Sources are mirrored incrementally into a per-project build directory so auxiliary files
//! never pollute the git repository and reruns stay fast. A global semaphore caps the number
//! of concurrent compiles, keeping the server's load predictable.

use crate::{
    auth::User,
    error::{AppError, AppResult},
    files::walk,
    state::{Role, SharedState},
    util::clean_rel_path,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashSet,
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::process::Command;

const MANIFEST: &str = ".lightleaf-sources";

fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p.join(bin).is_file()))
        .unwrap_or(false)
}

struct Tools {
    latexmk: bool,
    synctex: bool,
    compilers: Vec<&'static str>,
}

fn tools() -> &'static Tools {
    static T: OnceLock<Tools> = OnceLock::new();
    T.get_or_init(|| Tools {
        latexmk: which("latexmk") && which("perl"),
        synctex: which("synctex"),
        compilers: ["pdflatex", "folio", "typst"]
            .into_iter()
            .filter(|c| which(c))
            .collect(),
    })
}

pub fn available_compilers() -> Vec<&'static str> {
    tools().compilers.clone()
}

/// XeLaTeX, LuaLaTeX and Tectonic are deliberately not supported. Returns the message shown
/// when a project (e.g. one imported from Overleaf) asks for one of them.
pub fn incompatible_engine(c: &str) -> Option<String> {
    let name = match c {
        "xelatex" => "XeLaTeX",
        "lualatex" => "LuaLaTeX",
        "tectonic" => "Tectonic",
        _ => return None,
    };
    Some(format!(
        "{name} is incompatible with Lightleaf. Switch this project to folio or pdflatex \
         (project menu → Settings → Compiler)."
    ))
}

pub fn is_known_compiler(c: &str) -> bool {
    matches!(c, "pdflatex" | "folio" | "typst")
}

/// Compiler for new LaTeX projects: folio when installed, pdflatex otherwise.
pub fn default_latex_compiler() -> &'static str {
    if tools().compilers.contains(&"folio") {
        "folio"
    } else {
        "pdflatex"
    }
}

/// Attached to every pdflatex compile: folio is the LaTeX compiler projects should use.
fn pdflatex_warning() -> Issue {
    let mut message = "pdflatex is deprecated here: switch this project to folio \
                       (project menu → Settings → Compiler)"
        .to_string();
    if !tools().compilers.contains(&"folio") {
        message.push_str(". Folio is not installed on this server yet; ask an administrator.");
    }
    Issue {
        level: "warning",
        file: None,
        line: None,
        message,
        context: String::new(),
    }
}

#[derive(Serialize, Debug, Clone)]
pub struct Issue {
    level: &'static str, // error | warning | typesetting
    file: Option<String>,
    line: Option<u32>,
    message: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    context: String,
}

/// Mirrors the project working tree into the build dir, copying only files whose source
/// changed since the last sync. The manifest records each source file's size and mtime, so
/// build outputs that share a name with a source file (e.g. a committed `.bbl`) are not
/// clobbered on every compile.
fn sync_tree(src: &FsPath, dst: &FsPath) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    let manifest = dst.join(MANIFEST);
    let old: std::collections::HashMap<String, String> = std::fs::read_to_string(&manifest)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            l.split_once('\t')
                .map(|(p, s)| (p.to_string(), s.to_string()))
        })
        .collect();
    let mut now = Vec::new();
    for (rel, full, meta) in walk(src) {
        let target = dst.join(&rel);
        if meta.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let stamp = format!("{}:{}", meta.len(), mtime);
        if old.get(&rel) != Some(&stamp) || !target.exists() {
            std::fs::copy(&full, &target)?;
        }
        now.push((rel, stamp));
    }
    let current: HashSet<&str> = now.iter().map(|(r, _)| r.as_str()).collect();
    for stale in old.keys().filter(|k| !current.contains(k.as_str())) {
        let _ = std::fs::remove_file(dst.join(stale));
    }
    now.sort();
    std::fs::write(
        manifest,
        now.iter()
            .map(|(r, s)| format!("{r}\t{s}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

struct RunResult {
    output: String,
    timed_out: bool,
}

async fn run(
    state: &SharedState,
    dir: &FsPath,
    argv: &[String],
    deadline: Instant,
) -> std::io::Result<RunResult> {
    run_env(state, dir, argv, &[], deadline).await
}

/// [`run`] with extra environment variables.
async fn run_env(
    state: &SharedState,
    dir: &FsPath,
    argv: &[String],
    env: &[(&str, String)],
    deadline: Instant,
) -> std::io::Result<RunResult> {
    let mut full: Vec<String> = state.cfg.compile_wrapper.clone();
    full.extend_from_slice(argv);
    let mut cmd = Command::new(&full[0]);
    cmd.envs(env.iter().map(|(k, v)| (*k, v.as_str())));
    cmd.args(&full[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Hardening for TeX: restricted shell escape only, restricted file access, unwrapped logs.
        .env("shell_escape", "p")
        .env("openout_any", "p")
        .env("openin_any", "p")
        .env("max_print_line", "10000")
        .env("error_line", "254")
        .env("half_error_line", "238")
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn()?;
    let pid = child.id();
    let remaining = deadline.saturating_duration_since(Instant::now());
    match tokio::time::timeout(remaining, child.wait_with_output()).await {
        Ok(out) => {
            let out = out?;
            let mut s = String::from_utf8_lossy(&out.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&out.stderr));
            Ok(RunResult {
                output: s,
                timed_out: false,
            })
        }
        Err(_) => {
            if let Some(pid) = pid {
                let _ = Command::new("kill")
                    .args(["-9", &format!("-{pid}")])
                    .status()
                    .await;
            }
            Ok(RunResult {
                output: String::new(),
                timed_out: true,
            })
        }
    }
}

fn stem(main: &str) -> String {
    main.rsplit_once('.')
        .map(|(s, _)| s.to_string())
        .unwrap_or_else(|| main.to_string())
}

/// The main file's directory relative to the project root (`""` for a main file at
/// the root).
fn main_dir(main: &str) -> &str {
    main.rsplit_once('/').map_or("", |(d, _)| d)
}

/// File lookup for TeX when the main file is in a subfolder, as on Overleaf and as folio
/// does it: TeX runs in the project root, so root-relative paths resolve, and the main
/// file's directory comes next on the search paths (its class files, `\input`s and
/// `.bib`s). The trailing `:` keeps the TeX Live defaults.
fn tex_search_env(main: &str) -> Vec<(&'static str, String)> {
    let d = main_dir(main);
    if d.is_empty() {
        return Vec::new();
    }
    let path = format!(".:{d}:");
    ["TEXINPUTS", "BIBINPUTS", "BSTINPUTS"]
        .into_iter()
        .map(|k| (k, path.clone()))
        .collect()
}

/// pdflatex writes its outputs to the current directory; with a main file in a
/// subfolder they must land next to it (`<stem>.pdf`, `.log`, `.synctex.gz`). TeX also
/// writes the `.aux` of every `\include`d file at that file's path below the output
/// directory, so the project's directories are mirrored there.
fn tex_output_dir(dir: &FsPath, main: &str) -> Option<String> {
    let d = main_dir(main);
    if d.is_empty() {
        return None;
    }
    let out = dir.join(d);
    let prefix = format!("{d}/");
    for (rel, _, meta) in walk(dir) {
        if meta.is_dir() && rel != d && !rel.starts_with(&prefix) {
            let _ = std::fs::create_dir_all(out.join(&rel));
        }
    }
    Some(d.to_string())
}

async fn compile_pdflatex(
    state: &SharedState,
    dir: &FsPath,
    main: &str,
    deadline: Instant,
) -> std::io::Result<(String, bool)> {
    let s = stem(main);
    let mut transcript = String::new();
    let env = tex_search_env(main);
    let outdir = tex_output_dir(dir, main);
    if tools().latexmk {
        let mut argv = [
            "latexmk",
            "-pdf",
            "-f",
            "-interaction=nonstopmode",
            "-file-line-error",
            "-synctex=1",
        ]
        .map(String::from)
        .to_vec();
        if let Some(o) = &outdir {
            argv.push(format!("-outdir={o}"));
        }
        argv.push(main.to_string());
        let r = run_env(state, dir, &argv, &env, deadline).await?;
        let log = std::fs::read_to_string(dir.join(format!("{s}.log")))
            .unwrap_or_else(|_| r.output.clone());
        return Ok((log, r.timed_out));
    }
    // Manual pipeline: pdflatex → (bibtex|biber) → pdflatex (until references settle).
    let mut tex = [
        "pdflatex",
        "-interaction=nonstopmode",
        "-file-line-error",
        "-synctex=1",
    ]
    .map(String::from)
    .to_vec();
    if let Some(o) = &outdir {
        tex.push(format!("-output-directory={o}"));
    }
    tex.push(main.to_string());
    let mut bib_done = false;
    for pass in 0..4 {
        let r = run_env(state, dir, &tex, &env, deadline).await?;
        if r.timed_out {
            return Ok((transcript, true));
        }
        let log = std::fs::read_to_string(dir.join(format!("{s}.log"))).unwrap_or(r.output);
        let mut rerun = log.contains("Rerun to get")
            || log.contains("Rerun LaTeX")
            || log.contains("(rerunfilecheck)");
        if !bib_done {
            bib_done = true;
            let aux = std::fs::read_to_string(dir.join(format!("{s}.aux"))).unwrap_or_default();
            if dir.join(format!("{s}.bcf")).exists() && which("biber") {
                let b = run_env(state, dir, &["biber".into(), s.clone()], &env, deadline).await?;
                transcript.push_str(&b.output);
                rerun = true;
            } else if aux.contains("\\bibdata") && which("bibtex") {
                let b = run_env(state, dir, &["bibtex".into(), s.clone()], &env, deadline).await?;
                transcript.push_str(&b.output);
                rerun = true;
            }
            if dir.join(format!("{s}.idx")).exists() && which("makeindex") {
                let _ = run(
                    state,
                    dir,
                    &["makeindex".into(), format!("{s}.idx")],
                    deadline,
                )
                .await?;
                rerun = true;
            }
        }
        if !rerun || pass == 3 {
            return Ok((log, false));
        }
    }
    Ok((transcript, false))
}

/// Compiles with folio (<https://github.com/flecart/folio>), a LaTeX compiler written in Rust.
/// Folio handles bibliographies in process and has no SyncTeX output. Documents that use
/// constructs folio does not support are handed to slimtex by folio itself
/// (`--fallback=auto`), which leaves an ordinary TeX log next to the PDF.
async fn compile_folio(
    state: &SharedState,
    dir: &FsPath,
    main: &str,
    deadline: Instant,
) -> std::io::Result<(String, bool, Vec<Issue>)> {
    let s = stem(main);
    // Outputs of an earlier run with another engine would be mistaken for this run's:
    // a stale .log for slimtex's, a stale .synctex.gz for a PDF it does not describe.
    for ext in ["log", "synctex.gz"] {
        let _ = tokio::fs::remove_file(dir.join(format!("{s}.{ext}"))).await;
    }
    let argv = ["folio", "--color=never", "-o", &format!("{s}.pdf"), main].map(String::from);
    let r = run(state, dir, &argv, deadline).await?;
    let mut issues = parse_folio(&r.output);
    let mut log = r.output;
    if let Ok(tex) = std::fs::read_to_string(dir.join(format!("{s}.log"))) {
        issues.extend(parse_latex_log(&tex));
        log.push('\n');
        log.push_str(&tex);
    }
    Ok((log, r.timed_out, issues))
}

/// Header line of a folio diagnostic: `error[E0103]: message`, `warning: message`,
/// `fatal: message`. Returns the severity label and the message.
fn folio_header(l: &str) -> Option<(&str, &str)> {
    let (head, msg) = l.split_once(": ")?;
    let (sev, code) = head.split_once('[').unwrap_or((head, "]"));
    let code_ok = code
        .strip_suffix(']')
        .is_some_and(|c| c.chars().all(|c| c.is_ascii_alphanumeric()));
    (matches!(sev, "fatal" | "error" | "warning" | "note") && code_ok).then_some((sev, msg))
}

/// Parses folio's rustc-style diagnostics:
///
/// ```text
/// error[E0103]: `\end{enumerate}` closes `\begin{itemize}`
///  --> main.tex:1:24
///   |
/// 1 | \begin{itemize}\item ok\end{enumerate}
///   |                        ^^^^^^^^^^^^^^^ expected `\end{itemize}`
/// ```
///
/// Notes are informational and stay in the log only.
fn parse_folio(output: &str) -> Vec<Issue> {
    let lines: Vec<&str> = output.lines().collect();
    let mut issues = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some((sev, msg)) = folio_header(l) else {
            continue;
        };
        let level = match sev {
            "fatal" | "error" => "error",
            "warning" => "warning",
            _ => continue,
        };
        let body: Vec<&str> = lines[i + 1..]
            .iter()
            .take_while(|b| !b.trim().is_empty() && folio_header(b).is_none())
            .copied()
            .collect();
        let loc = body
            .iter()
            .find_map(|b| b.trim_start().strip_prefix("--> "));
        let (file, line) = loc
            .and_then(|loc| {
                let mut p = loc.rsplitn(3, ':');
                let _col = p.next();
                let line = p.next()?.parse().ok()?;
                Some((
                    Some(p.next()?.trim_start_matches("./").to_string()),
                    Some(line),
                ))
            })
            .unwrap_or((None, None));
        issues.push(Issue {
            level,
            file,
            line,
            message: msg.to_string(),
            context: body
                .iter()
                .filter(|b| !b.trim_start().starts_with("--> "))
                .copied()
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }
    issues
}

fn parse_latex_log(log: &str) -> Vec<Issue> {
    let lines: Vec<&str> = log.lines().collect();
    let mut issues: Vec<Issue> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        // file:line: message   (thanks to -file-line-error)
        if let Some((file, rest)) = split_file_line(l) {
            let (line, msg) = rest;
            let ctx = lines
                .get(i + 1..(i + 3).min(lines.len()))
                .map(|c| c.join("\n"))
                .unwrap_or_default();
            issues.push(Issue {
                level: "error",
                file: Some(file),
                line: Some(line),
                message: msg.to_string(),
                context: ctx,
            });
        } else if let Some(msg) = l.strip_prefix("! ") {
            let mut line = None;
            for next in lines.iter().skip(i + 1).take(12) {
                if let Some(n) = next.strip_prefix("l.") {
                    line = n
                        .split(|c: char| !c.is_ascii_digit())
                        .next()
                        .and_then(|d| d.parse().ok());
                    break;
                }
            }
            issues.push(Issue {
                level: "error",
                file: None,
                line,
                message: msg.to_string(),
                context: String::new(),
            });
        } else if l.contains("Warning:")
            && (l.starts_with("LaTeX")
                || l.starts_with("Package")
                || l.starts_with("Class")
                || l.starts_with("pdfTeX"))
        {
            let mut msg = l.to_string();
            let mut j = i + 1;
            while j < lines.len()
                && !lines[j].trim().is_empty()
                && lines[j].starts_with(' ')
                && j < i + 6
            {
                msg.push(' ');
                msg.push_str(lines[j].trim());
                j += 1;
            }
            let line = msg
                .split("input line ")
                .nth(1)
                .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|d| d.parse().ok());
            issues.push(Issue {
                level: "warning",
                file: None,
                line,
                message: msg,
                context: String::new(),
            });
        } else if l.starts_with("Overfull \\") || l.starts_with("Underfull \\") {
            let line = l
                .split(" at line")
                .nth(1)
                .map(|r| r.trim_start_matches('s').trim())
                .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|d| d.parse().ok());
            issues.push(Issue {
                level: "typesetting",
                file: None,
                line,
                message: l.to_string(),
                context: String::new(),
            });
        }
        i += 1;
    }
    // De-duplicate "! " errors that duplicate a file:line: error.
    let mut seen = HashSet::new();
    issues.retain(|is| seen.insert((is.level, is.line, is.message.clone())));
    issues
}

fn split_file_line(l: &str) -> Option<(String, (u32, &str))> {
    let mut parts = l.splitn(3, ':');
    let file = parts.next()?;
    let line = parts.next()?;
    let msg = parts.next()?.trim();
    if file.is_empty()
        || file.contains(' ')
        || !file.contains('.')
        || line.is_empty()
        || !line.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let file = file.trim_start_matches("./").to_string();
    Some((file, (line.parse().ok()?, msg)))
}

fn parse_typst(output: &str) -> Vec<Issue> {
    output
        .lines()
        .filter_map(|l| {
            let (level, idx, sep) = if let Some(i) = l.find(": error: ") {
                ("error", i, ": error: ")
            } else {
                let i = l.find(": warning: ")?;
                ("warning", i, ": warning: ")
            };
            let loc = &l[..idx];
            let msg = &l[idx + sep.len()..];
            let mut p = loc.rsplitn(3, ':');
            let _col = p.next();
            let line = p.next().and_then(|n| n.parse().ok());
            let file = p.next().map(|f| f.trim_start_matches("./").to_string());
            Some(Issue {
                level,
                file,
                line,
                message: msg.to_string(),
                context: String::new(),
            })
        })
        .collect()
}

#[derive(Deserialize, Default)]
pub struct CompileReq {
    #[serde(default)]
    pub main_file: Option<String>,
}

pub async fn compile(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    body: Option<Json<CompileReq>>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    let pid = id.clone();
    let (compiler, main_file): (String, String) = state
        .db(move |c| {
            c.query_row(
                "SELECT compiler, main_file FROM projects WHERE id = ?1",
                [pid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await?;
    let main = match body.and_then(|b| b.0.main_file) {
        Some(m) if !m.is_empty() => clean_rel_path(&m)?,
        _ => clean_rel_path(&main_file)?,
    };
    if let Some(msg) = incompatible_engine(&compiler) {
        return Err(AppError::bad(msg));
    }
    if !tools().compilers.contains(&compiler.as_str()) {
        return Err(AppError::bad(format!(
            "compiler '{compiler}' is not installed on this server"
        )));
    }
    let src = state.cfg.project_dir(&id);
    let build = state.cfg.project_build_dir(&id);
    if !src.join(&main).is_file() {
        return Err(AppError::bad(format!(
            "main file '{main}' does not exist — set it in the project menu"
        )));
    }

    // Fairness: one user cannot fill the global compile queue.
    let _user_slot = state.limits.user_compile_slot(user.id).ok_or_else(|| {
        crate::metrics::Metrics::inc(&state.metrics.compiles_rejected);
        AppError(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "you already have compiles running — wait for them to finish".into(),
        )
    })?;

    // One compile per project at a time; queued globally by the semaphore.
    let arrived = Instant::now();
    let build_lock = state.project_lock(&format!("build:{id}"));
    let _b = build_lock.lock().await;
    // Coalescing: if a compile of the same document started after this request arrived, it
    // already includes every edit this request could have wanted, so reuse its result. With
    // auto-compile and several collaborators, a burst of N requests costs at most two compiles.
    let key = format!("{id}\0{compiler}\0{main}");
    if let Some(result) = state.recent_compile(&key, arrived) {
        crate::metrics::Metrics::inc(&state.metrics.compiles_coalesced);
        return Ok(Json(result));
    }
    if let Some(wait) = state.limits.compiles_per_user.blocked(&user.id.to_string()) {
        crate::metrics::Metrics::inc(&state.metrics.compiles_rejected);
        return Err(AppError(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            format!("compile limit reached — try again in {wait} s"),
        ));
    }
    state.limits.compiles_per_user.hit(&user.id.to_string());
    let started = Instant::now();
    {
        let lock = state.project_lock(&id);
        let _g = lock.lock().await;
        if let Some(room) = state.hub.get(&id) {
            room.flush_locked().await?;
        }
        let (s, b) = (src.clone(), build.clone());
        tokio::task::spawn_blocking(move || sync_tree(&s, &b))
            .await
            .map_err(anyhow::Error::from)??;
    }
    let _permit = state
        .compile_slots
        .acquire()
        .await
        .map_err(anyhow::Error::from)?;
    let compile_started = Instant::now();
    let deadline = compile_started + Duration::from_secs(state.cfg.compile_timeout_secs);
    let s = stem(&main);
    let pdf = build.join(format!("{s}.pdf"));
    let _ = tokio::fs::remove_file(&pdf).await;

    let (log, timed_out, issues) = if compiler == "typst" {
        let r = run(
            &state,
            &build,
            &[
                "typst",
                "compile",
                "--root",
                ".",
                "--diagnostic-format",
                "short",
                &main,
                &format!("{s}.pdf"),
            ]
            .map(String::from),
            deadline,
        )
        .await?;
        let issues = parse_typst(&r.output);
        (r.output, r.timed_out, issues)
    } else if compiler == "folio" {
        compile_folio(&state, &build, &main, deadline).await?
    } else {
        let (log, t) = compile_pdflatex(&state, &build, &main, deadline).await?;
        let mut issues = parse_latex_log(&log);
        issues.insert(0, pdflatex_warning());
        (log, t, issues)
    };
    let elapsed = compile_started.elapsed().as_millis() as u64;
    let ok = tokio::fs::metadata(&pdf).await.is_ok();
    if ok {
        tokio::fs::copy(&pdf, build.join(".output.pdf")).await?;
    }
    let mut full_log = log;
    if timed_out {
        full_log.push_str(&format!(
            "\n\n[Lightleaf] Compilation timed out after {}s.\n",
            state.cfg.compile_timeout_secs
        ));
    }
    tokio::fs::write(build.join(".output.log"), &full_log).await?;
    let has_errors = issues.iter().any(|i| i.level == "error");
    let status = if timed_out {
        "timeout"
    } else if ok && !has_errors {
        "success"
    } else if ok {
        "errors"
    } else {
        "failure"
    };
    state.metrics.record_compile(status, elapsed);
    let result = json!({
        "status": status,
        "durationMs": elapsed,
        "pdf": if ok { Some(format!("/api/projects/{id}/output/output.pdf?t={}", crate::util::now_ms())) } else { None },
        "issues": issues,
        "log": full_log,
        "synctex": !matches!(compiler.as_str(), "typst" | "folio") && tools().synctex,
        "mainFile": main,
    });
    state.remember_compile(key, started, result.clone());
    Ok(Json(result))
}

pub async fn output(
    State(state): State<SharedState>,
    user: User,
    Path((id, name)): Path<(String, String)>,
) -> AppResult<Response> {
    state.require(&id, user.id, Role::Viewer).await?;
    let (file, mime) = match name.as_str() {
        "output.pdf" => (".output.pdf", "application/pdf"),
        "output.log" => (".output.log", "text/plain; charset=utf-8"),
        _ => return Err(AppError::not_found()),
    };
    let bytes = tokio::fs::read(state.cfg.project_build_dir(&id).join(file))
        .await
        .map_err(|_| AppError::not_found())?;
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, max-age=3600"),
        ],
        bytes,
    )
        .into_response())
}

async fn main_pdf(state: &SharedState, id: &str) -> AppResult<(PathBuf, PathBuf)> {
    if !tools().synctex {
        return Err(AppError::bad("synctex is not installed"));
    }
    let pid = id.to_string();
    let main: String = state
        .db(move |c| {
            c.query_row("SELECT main_file FROM projects WHERE id = ?1", [pid], |r| {
                r.get(0)
            })
        })
        .await?;
    let build = std::fs::canonicalize(state.cfg.project_build_dir(id))
        .map_err(|_| AppError::not_found())?;
    Ok((build.join(format!("{}.pdf", stem(&main))), build))
}

#[derive(Deserialize)]
pub struct PdfPos {
    page: u32,
    h: f64,
    v: f64,
}

/// PDF position → source location (double click in the PDF).
pub async fn synctex_from_pdf(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Query(q): Query<PdfPos>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    let (pdf, build) = main_pdf(&state, &id).await?;
    let out = Command::new("synctex")
        .args([
            "edit",
            "-o",
            &format!("{}:{:.2}:{:.2}:{}", q.page, q.h, q.v, pdf.display()),
        ])
        .current_dir(&build)
        .output()
        .await?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut file = None;
    let mut line = None;
    for l in text.lines() {
        if let Some(f) = l.strip_prefix("Input:") {
            let f = f.trim();
            let rel = f
                .strip_prefix(&*build.to_string_lossy())
                .unwrap_or(f)
                .trim_start_matches('/');
            file = Some(rel.trim_start_matches("./").replace("/./", "/"));
        } else if let Some(n) = l.strip_prefix("Line:") {
            line = n.trim().parse::<u32>().ok();
            break;
        }
    }
    match (file, line) {
        (Some(f), Some(l)) => Ok(Json(json!({ "file": f, "line": l }))),
        _ => Err(AppError::not_found()),
    }
}

#[derive(Deserialize)]
pub struct CodePos {
    file: String,
    line: u32,
    #[serde(default)]
    column: u32,
}

/// Source location → PDF position (jump to PDF).
pub async fn synctex_from_code(
    State(state): State<SharedState>,
    user: User,
    Path(id): Path<String>,
    Query(q): Query<CodePos>,
) -> AppResult<Json<serde_json::Value>> {
    state.require(&id, user.id, Role::Viewer).await?;
    let (pdf, build) = main_pdf(&state, &id).await?;
    let rel = clean_rel_path(&q.file)?;
    let input = build.join(&rel);
    let out = Command::new("synctex")
        .args([
            "view",
            "-i",
            &format!("{}:{}:{}", q.line, q.column, input.display()),
            "-o",
            &pdf.to_string_lossy(),
        ])
        .current_dir(&build)
        .output()
        .await?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut res = serde_json::Map::new();
    for l in text.lines() {
        if let Some((k, v)) = l.split_once(':') {
            let key = match k {
                "Page" => "page",
                "h" => "h",
                "v" => "v",
                "W" => "width",
                "H" => "height",
                _ => continue,
            };
            if let Ok(n) = v.trim().parse::<f64>() {
                res.insert(key.to_string(), json!(n));
            }
            if k == "H" {
                break; // first record is enough
            }
        }
    }
    if res.contains_key("page") {
        Ok(Json(serde_json::Value::Object(res)))
    } else {
        Err(AppError::not_found())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tex_finds_root_and_main_dir_files() {
        assert_eq!(main_dir("main.tex"), "");
        assert_eq!(main_dir("paper/neurips/main.tex"), "paper/neurips");
        assert!(tex_search_env("main.tex").is_empty());
        let env = tex_search_env("neurips26/main.tex");
        assert_eq!(env.len(), 3);
        assert!(env.iter().all(|(_, v)| v == ".:neurips26:"), "{env:?}");

        let dir = std::env::temp_dir().join(format!("ll-outdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("neurips26")).unwrap();
        std::fs::create_dir_all(dir.join("body/figs")).unwrap();
        assert_eq!(tex_output_dir(&dir, "main.tex"), None);
        assert_eq!(
            tex_output_dir(&dir, "neurips26/main.tex").as_deref(),
            Some("neurips26")
        );
        assert!(dir.join("neurips26/body/figs").is_dir());
        assert!(!dir.join("neurips26/neurips26").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn xelatex_and_lualatex_are_incompatible() {
        for c in ["xelatex", "lualatex", "tectonic"] {
            let m = incompatible_engine(c).unwrap();
            assert!(
                m.contains("incompatible with Lightleaf") && m.contains("folio or pdflatex"),
                "{m}"
            );
            assert!(!is_known_compiler(c));
        }
        assert!(
            incompatible_engine("pdflatex").is_none() && incompatible_engine("folio").is_none()
        );
    }

    #[test]
    fn parses_file_line_errors() {
        let log = "./main.tex:12: Undefined control sequence.\nl.12 \\foo\n\nLaTeX Warning: Reference `x' on page 1 undefined on input line 7.\n\nOverfull \\hbox (1.0pt too wide) in paragraph at lines 3--4\n";
        let is = parse_latex_log(log);
        assert_eq!(is[0].file.as_deref(), Some("main.tex"));
        assert_eq!(is[0].line, Some(12));
        assert!(is.iter().any(|i| i.level == "warning" && i.line == Some(7)));
        assert!(
            is.iter()
                .any(|i| i.level == "typesetting" && i.line == Some(3))
        );
    }

    #[test]
    fn parses_typst() {
        let is = parse_typst(
            "main.typ:3:5: error: unknown variable: foo\nchap/a.typ:10:1: warning: unused",
        );
        assert_eq!(is.len(), 2);
        assert_eq!(is[0].line, Some(3));
        assert_eq!(is[1].file.as_deref(), Some("chap/a.typ"));
    }

    #[test]
    fn parses_folio() {
        let out = "\
note: compiling `main.tex` with slimtex (--fallback=auto) because it uses constructs
error[E0103]: `\\end{enumerate}` closes `\\begin{itemize}`
 --> ./main.tex:12:24
  |
12 | \\begin{itemize}\\item ok\\end{enumerate}
  |                        ^^^^^^^^^^^^^^^ expected `\\end{itemize}`

warning[W0201]: unknown package `foo`; ignored

note[N0001]: just so you know

fatal: cannot read `x.tex`: No such file or directory
main.tex: 2 error(s), 1 warning(s)
";
        let is = parse_folio(out);
        assert_eq!(is.len(), 3, "{is:?}");
        assert_eq!(is[0].level, "error");
        assert_eq!(is[0].file.as_deref(), Some("main.tex"));
        assert_eq!(is[0].line, Some(12));
        assert!(is[0].context.contains("^^^"));
        assert!(!is[0].context.contains("-->"));
        assert_eq!((is[1].level, is[1].line), ("warning", None));
        assert_eq!(is[1].message, "unknown package `foo`; ignored");
        assert_eq!(is[2].level, "error");
        assert!(is[2].message.starts_with("cannot read"));
    }
}
