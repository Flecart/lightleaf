//! The document's source files: the main file and its `\input`/`\include`/`\subfile`s.

use super::code_part;
use std::{
    collections::HashSet,
    path::{Component, Path},
};

/// A source file of the document.
pub struct SourceFile {
    pub path: String,
    pub text: String,
}

/// The document's `.tex` files in reading order (main file first, then `\input`s depth-first).
pub struct Document {
    pub files: Vec<SourceFile>,
}

impl Document {
    /// All files concatenated with markers, as shown to the model.
    pub fn merged(&self) -> String {
        let mut out = String::new();
        for f in &self.files {
            out.push_str(&format!("%%%%% FILE: {} %%%%%\n", f.path));
            out.push_str(&f.text);
            if !f.text.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }

    pub fn total_len(&self) -> usize {
        self.files.iter().map(|f| f.text.len()).sum()
    }
}

/// Resolves an `\input` argument to a safe project-relative `.tex` path.
fn resolve_input(arg: &str) -> Option<String> {
    let arg = arg.trim().trim_matches('"');
    if arg.is_empty() {
        return None;
    }
    let p = Path::new(arg);
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let mut s = p
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    if Path::new(&s).extension().is_none() {
        s.push_str(".tex");
    }
    s.ends_with(".tex").then_some(s)
}

/// Arguments of `\input{…}`, `\include{…}` and `\subfile{…}` outside comments.
fn included_files(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let code = code_part(line);
        for cmd in ["\\input{", "\\include{", "\\subfile{"] {
            let mut rest = code;
            while let Some(i) = rest.find(cmd) {
                let after = &rest[i + cmd.len()..];
                if let Some(end) = after.find('}')
                    && let Some(p) = resolve_input(&after[..end])
                {
                    out.push(p);
                }
                rest = after;
            }
        }
    }
    out
}

/// Reads the main file and everything it includes (depth-first, each file once).
pub fn gather(root: &Path, main: &str) -> anyhow::Result<Document> {
    let main = resolve_input(main).ok_or_else(|| anyhow::anyhow!("invalid main file {main:?}"))?;
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = vec![(main.clone(), 0usize)];
    while let Some((path, depth)) = stack.pop() {
        if depth > 20 || !seen.insert(path.clone()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&path)) else {
            if path == main {
                anyhow::bail!("main file {main} not found or not UTF-8");
            }
            tracing::debug!("skipping missing include {path}");
            continue;
        };
        let children = included_files(&text);
        files.push(SourceFile { path, text });
        for c in children.into_iter().rev() {
            stack.push((c, depth + 1));
        }
    }
    Ok(Document { files })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_includes_safely() {
        assert_eq!(
            included_files("\\input{sections/intro}\n% \\input{old}\n\\include{b.tex}"),
            ["sections/intro.tex", "b.tex"]
        );
        assert!(included_files("\\input{../../etc/passwd}").is_empty());
        assert!(included_files("\\input{/abs}").is_empty());
    }
}
