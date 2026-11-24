//! Repository tasks: `cargo xtask lint [--fix-comments] [--list-comments] [--no-clippy]`.
//!
//! **Lint tiers.** `[workspace.lints]` in the root `Cargo.toml` sets every group to `warn`;
//! this task decides what is mandatory per crate:
//! - core crates: rustc warnings, `clippy::correctness` and `clippy::suspicious` are errors
//!   (`-D warnings` with the advisory groups allowed), then a second pass shows the advisory
//!   groups (`style`, `complexity`, `perf`, `pedantic`) as warnings;
//! - other crates: everything is a warning.
//!
//! **Comment rule** (see [`comments`]): plain comments without an allowed marker are errors in
//! core crates and warnings elsewhere. `--fix-comments` deletes them, writing a file only when
//! `syn` sees the same code before and after, then runs `rustfmt --check` on what it changed.
//! `--list-comments` prints every plain comment with its verdict, for review.

mod comments;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// A workspace crate, where its sources live, and whether it is core.
struct Crate {
    name: &'static str,
    /// Directories or files, relative to the workspace root.
    paths: &'static [&'static str],
    core: bool,
}

/// The core crates are the ones the README presents: the server and the AI reviewer.
const CRATES: &[Crate] = &[
    Crate {
        name: "lightleaf",
        paths: &["src", "tests", "benches", "build.rs"],
        core: true,
    },
    Crate {
        name: "lightleaf-reviewer",
        paths: &[
            "reviewer/src",
            "reviewer/tests",
            "reviewer/benches",
            "reviewer/build.rs",
        ],
        core: true,
    },
    Crate {
        name: "xtask",
        paths: &["xtask/src"],
        core: false,
    },
];

/// Files whose comments are data or that are generated: the comment rule skips them.
/// Files with `@generated` in their first lines are skipped too.
const EXEMPT: &[&str] = &["reviewer/src/review/packs/papermentor/files.rs"];

/// Groups that never fail the build: allowed in the mandatory pass, warned in the advisory one.
const ADVISORY: &[&str] = &[
    "clippy::style",
    "clippy::complexity",
    "clippy::perf",
    "clippy::pedantic",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some("lint") = args.first().map(String::as_str) {
        lint(&args[1..])
    } else {
        eprintln!("usage: cargo xtask lint [--fix-comments] [--list-comments] [--no-clippy]");
        ExitCode::from(2)
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}

fn lint(args: &[String]) -> ExitCode {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if let Some(bad) = args
        .iter()
        .find(|a| !["--fix-comments", "--list-comments", "--no-clippy"].contains(&a.as_str()))
    {
        eprintln!("unknown option `{bad}`");
        return ExitCode::from(2);
    }
    let root = root();
    let mut ok = true;
    if !has("--no-clippy") {
        for c in CRATES {
            ok &= clippy(&root, c);
        }
    }
    let mut changed = Vec::new();
    for c in CRATES {
        ok &= comment_rule(
            &root,
            c,
            has("--fix-comments"),
            has("--list-comments"),
            &mut changed,
        );
    }
    if !changed.is_empty() {
        ok &= rustfmt_check(&root, &changed);
    }
    if ok {
        println!("lint: ok");
        ExitCode::SUCCESS
    } else {
        println!("lint: FAILED (mandatory findings above)");
        ExitCode::FAILURE
    }
}

fn cargo_clippy(root: &Path, krate: &str, extra: &[String]) -> bool {
    let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.current_dir(root)
        .args(["clippy", "--quiet", "-p", krate, "--all-targets", "--"])
        .args(extra);
    cmd.status().is_ok_and(|s| s.success())
}

/// Runs the clippy tiers for one crate; false when a mandatory finding fails it.
fn clippy(root: &Path, c: &Crate) -> bool {
    if !c.core {
        println!("== clippy {} (advisory)", c.name);
        if !cargo_clippy(root, c.name, &[]) {
            println!(
                "warning: clippy reported errors in non-core crate {}",
                c.name
            );
        }
        return true;
    }
    println!(
        "== clippy {} (mandatory: rustc warnings, correctness, suspicious)",
        c.name
    );
    let mut deny = vec!["-D".to_string(), "warnings".to_string()];
    for g in ADVISORY {
        deny.extend(["-A".to_string(), (*g).to_string()]);
    }
    if !cargo_clippy(root, c.name, &deny) {
        return false;
    }
    println!(
        "== clippy {} (advisory: style, complexity, perf, pedantic)",
        c.name
    );
    cargo_clippy(root, c.name, &[]);
    true
}

/// Every `.rs` file under `path` (a file or a directory), skipping `target` directories.
fn rust_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|e| e == "rs") {
            out.push(path.to_path_buf());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.file_name().is_some_and(|n| n == "target") {
            continue;
        }
        rust_files(&p, out);
    }
}

fn is_generated(src: &str) -> bool {
    src.lines().take(5).any(|l| l.contains("@generated"))
}

/// Applies the comment rule to one crate; false when a core crate keeps a refused comment.
fn comment_rule(root: &Path, c: &Crate, fix: bool, list: bool, changed: &mut Vec<PathBuf>) -> bool {
    let mut files = Vec::new();
    for p in c.paths {
        rust_files(&root.join(p), &mut files);
    }
    let level = if c.core { "error" } else { "warning" };
    let mut ok = true;
    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        if EXEMPT.contains(&rel.as_str()) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&file) else {
            println!("{level}: {rel}: cannot read");
            ok &= !c.core;
            continue;
        };
        if is_generated(&src) {
            continue;
        }
        let found = comments::scan(&src);
        if list {
            for cm in &found {
                let verdict = if cm.allowed { "allowed" } else { "refused" };
                println!("{rel}:{}: [{verdict}] {}", cm.line, first_line(&cm.text));
            }
        }
        let refused: Vec<&comments::Comment> = found.iter().filter(|cm| !cm.allowed).collect();
        if refused.is_empty() {
            continue;
        }
        if fix && let Some(new) = comments::strip(&src, &found) {
            match comments::same_code(&src, &new) {
                Ok(true) => {
                    if std::fs::write(&file, new).is_ok() {
                        println!("fixed: {rel}: removed {} comment(s)", refused.len());
                        changed.push(file);
                        continue;
                    }
                    println!("{level}: {rel}: cannot write");
                }
                Ok(false) => println!("{level}: {rel}: not written, the code would change"),
                Err(e) => println!("{level}: {rel}: not written, does not parse: {e}"),
            }
        }
        if !list {
            for cm in &refused {
                println!(
                    "{level}: {rel}:{}: comment needs a marker (keep:, SAFETY:, TODO(name):) or a doc comment: {}",
                    cm.line,
                    first_line(&cm.text)
                );
            }
        }
        ok &= !c.core;
    }
    ok
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim_end()
}

fn rustfmt_check(root: &Path, files: &[PathBuf]) -> bool {
    println!("== rustfmt --check on {} rewritten file(s)", files.len());
    Command::new("rustfmt")
        .current_dir(root)
        .args(["--edition", "2024", "--check"])
        .args(files)
        .status()
        .is_ok_and(|s| s.success())
}
