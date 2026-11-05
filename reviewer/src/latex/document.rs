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
    /// The main file with every `\\input`/`\\include`/`\\subfile` replaced in place by the
    /// file's content between marker comments (missing or repeated files leave a marker), so
    /// the text reads in document order.
    pub fn inlined(&self) -> String {
        let Some(main) = self.files.first() else {
            return String::new();
        };
        let mut seen = HashSet::from([main.path.clone()]);
        self.inline_into(&main.text, &mut seen, 0)
    }

    fn inline_into(&self, text: &str, seen: &mut HashSet<String>, depth: usize) -> String {
        let mut out = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            let code = code_part(line);
            let mut rest_at = 0;
            let mut replaced = String::new();
            let mut cursor = 0;
            while let Some((start, end, path)) =
                next_include(&code[rest_at..]).map(|(s, e, p)| (rest_at + s, rest_at + e, p))
            {
                replaced.push_str(&line[cursor..start]);
                let body = match self.files.iter().find(|f| f.path == path) {
                    Some(_) if !seen.insert(path.clone()) || depth > 20 => {
                        format!("% [AI Tutor] Could not inline: {path} (already included)")
                    }
                    Some(f) => format!(
                        "% ========== INLINED FROM: {path} ==========\n{}\n% ========== END OF: {path} ==========",
                        self.inline_into(&f.text, seen, depth + 1)
                    ),
                    None => format!("% [AI Tutor] Could not inline: {path} (not found)"),
                };
                replaced.push_str(&body);
                cursor = end;
                rest_at = end;
            }
            if cursor == 0 {
                out.push_str(line);
            } else {
                replaced.push_str(&line[cursor..]);
                out.push_str(&replaced);
            }
        }
        out
    }

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

/// The first `\\input{…}`/`\\include{…}`/`\\subfile{…}` in `code`: (start, end, resolved path).
fn next_include(code: &str) -> Option<(usize, usize, String)> {
    ["\\input{", "\\include{", "\\subfile{"]
        .iter()
        .filter_map(|cmd| {
            let i = code.find(cmd)?;
            let close = code[i + cmd.len()..].find('}')? + i + cmd.len();
            let path = resolve_input(&code[i + cmd.len()..close])?;
            Some((i, close + 1, path))
        })
        .min_by_key(|(i, _, _)| *i)
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
    fn inlines_includes_in_document_order() {
        let doc = Document {
            files: vec![
                SourceFile {
                    path: "main.tex".into(),
                    text: "A\n\\input{b}\nC % \\input{x}\n\\input{missing}\n".into(),
                },
                SourceFile {
                    path: "b.tex".into(),
                    text: "B1\nB2".into(),
                },
            ],
        };
        let t = doc.inlined();
        assert!(t.starts_with("A\n% ========== INLINED FROM: b.tex ==========\nB1\nB2\n% ========== END OF: b.tex ==========\nC"));
        assert!(t.contains("% [AI Tutor] Could not inline: missing.tex (not found)"));
    }

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
