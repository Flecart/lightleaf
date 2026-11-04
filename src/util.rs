use crate::error::{AppError, AppResult};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

pub fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Normalises a user supplied project-relative path. Rejects absolute paths, `..`,
/// and anything touching the `.git` directory.
pub fn clean_rel_path(p: &str) -> AppResult<String> {
    let p = p.trim().trim_start_matches('/');
    if p.is_empty() {
        return Err(AppError::bad("empty path"));
    }
    let mut parts = Vec::new();
    for c in Path::new(p).components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str().ok_or_else(|| AppError::bad("invalid path"))?;
                if s == ".git" || s.contains('\\') || s.contains('\0') {
                    return Err(AppError::bad("invalid path"));
                }
                parts.push(s.to_string());
            }
            Component::CurDir => {}
            _ => return Err(AppError::bad("invalid path")),
        }
    }
    if parts.is_empty() {
        return Err(AppError::bad("empty path"));
    }
    Ok(parts.join("/"))
}

pub fn join_safe(root: &Path, rel: &str) -> AppResult<(String, PathBuf)> {
    let rel = clean_rel_path(rel)?;
    let full = root.join(&rel);
    Ok((rel, full))
}

/// Heuristic: is this content an editable text document?
pub fn is_text(bytes: &[u8]) -> bool {
    bytes.len() <= 4 * 1024 * 1024 && !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

pub fn is_text_ext(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "tex" | "typ" | "bib" | "cls" | "sty" | "bst" | "txt" | "md" | "csv" | "tsv" | "json" | "yaml" | "yml"
            | "toml" | "ltx" | "dtx" | "ins" | "bbx" | "cbx" | "lbx" | "def" | "cfg" | "clo" | "gitignore"
            | "latexmkrc" | "tikz" | "asy" | "py" | "r" | "lua" | "sh" | "xml" | "svg" | "html" | "css" | "js"
    )
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
