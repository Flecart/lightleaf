//! LaTeX source handling: collecting the files of a document, splitting it into sections,
//! anchoring quotes back to source positions, and inserting/removing comment macros at
//! positions where they cannot break compilation.

use std::{
    collections::HashSet,
    path::{Component, Path},
};

/// Name of the package the reviewer adds to projects (`lightleaf-comments.sty`).
pub const PACKAGE: &str = "lightleaf-comments";

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

/// Index of the first unescaped `%` in `line`, if any.
fn comment_start(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'%' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// `line` without its comment.
fn code_part(line: &str) -> &str {
    &line[..comment_start(line).unwrap_or(line.len())]
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

/// A section of the merged document, as given to the classifier.
#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub in_appendix: bool,
    /// Byte range in the merged text.
    pub start: usize,
    pub end: usize,
}

/// Reads a balanced `{…}` group starting at `s[0] == '{'`; returns (content, bytes consumed).
fn braced(s: &str) -> Option<(&str, usize)> {
    let b = s.as_bytes();
    if b.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&s[1..i], i + 1));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Splits the merged document into an abstract and top-level sections (`\chapter`/`\section`).
pub fn sections(merged: &str) -> Vec<Section> {
    let mut marks: Vec<(usize, String, bool)> = Vec::new();
    let mut appendix = false;
    let mut offset = 0;
    for line in merged.split_inclusive('\n') {
        let code = code_part(line);
        if code.contains("\\appendix") {
            appendix = true;
        }
        if let Some(i) = code.find("\\begin{abstract}") {
            marks.push((offset + i, "Abstract".into(), false));
        }
        for cmd in ["\\chapter", "\\section"] {
            let mut search = 0;
            while let Some(i) = code[search..].find(cmd) {
                let at = search + i;
                // Read from the whole text: titles may continue on the next line.
                let mut rest = &merged[offset + at + cmd.len()..];
                search = at + cmd.len();
                // Skip \sectionmark, \section@… and friends.
                if rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '@') {
                    continue;
                }
                rest = rest.strip_prefix('*').unwrap_or(rest).trim_start();
                if rest.starts_with('[')
                    && let Some(close) = rest.find(']')
                {
                    rest = rest[close + 1..].trim_start();
                }
                if let Some((title, _)) = braced(rest) {
                    marks.push((
                        offset + at,
                        title.split_whitespace().collect::<Vec<_>>().join(" "),
                        appendix,
                    ));
                }
            }
        }
        offset += line.len();
    }
    marks.sort_by_key(|m| m.0);
    let mut out: Vec<Section> = Vec::new();
    for (k, (start, title, in_appendix)) in marks.iter().enumerate() {
        let mut end = marks.get(k + 1).map(|m| m.0).unwrap_or(merged.len());
        if title == "Abstract"
            && let Some(e) = merged[*start..].find("\\end{abstract}")
        {
            end = end.min(start + e + "\\end{abstract}".len());
        }
        out.push(Section {
            title: title.clone(),
            in_appendix: *in_appendix,
            start: *start,
            end,
        });
    }
    out
}

/// Whitespace-collapsed copy of `s` with, for every byte of the copy, the source byte range.
fn normalize(s: &str) -> (String, Vec<(usize, usize)>) {
    let mut out = String::with_capacity(s.len());
    let mut map = Vec::with_capacity(s.len());
    let mut in_ws = false;
    for (i, c) in s.char_indices() {
        let end = i + c.len_utf8();
        if c.is_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push(' ');
                map.push((i, end));
            } else if let Some(last) = map.last_mut() {
                last.1 = end;
            }
            in_ws = true;
        } else {
            in_ws = false;
            let mut buf = [0u8; 4];
            for _ in c.encode_utf8(&mut buf).bytes() {
                map.push((i, end));
            }
            out.push(c);
        }
    }
    (out, map)
}

/// Finds `quote` in `text` (exactly, or ignoring differences in whitespace). Returns the byte range.
pub fn find_quote(text: &str, quote: &str) -> Option<(usize, usize)> {
    let quote = quote.trim();
    if quote.chars().count() < 8 {
        return None;
    }
    if let Some(i) = text.find(quote) {
        return Some((i, i + quote.len()));
    }
    let (nt, map) = normalize(text);
    let (nq, _) = normalize(quote);
    let i = nt.find(nq.trim())?;
    let j = i + nq.trim().len();
    Some((map[i].0, map[j - 1].1))
}

/// Environments inside which an inline comment could break compilation or would be invisible.
const UNSAFE_ENVS: &[&str] = &[
    "equation",
    "equation*",
    "align",
    "align*",
    "alignat",
    "alignat*",
    "gather",
    "gather*",
    "multline",
    "multline*",
    "flalign",
    "flalign*",
    "eqnarray",
    "eqnarray*",
    "displaymath",
    "math",
    "split",
    "cases",
    "array",
    "matrix",
    "pmatrix",
    "bmatrix",
    "tabular",
    "tabular*",
    "tabularx",
    "tabulary",
    "longtable",
    "supertabular",
    "tikzpicture",
    "picture",
    "verbatim",
    "verbatim*",
    "Verbatim",
    "lstlisting",
    "minted",
    "comment",
    "figure",
    "figure*",
    "table",
    "table*",
    "algorithm",
    "algorithmic",
    "algorithm2e",
    "thebibliography",
    "filecontents",
    "filecontents*",
    "subfigure",
    "wrapfigure",
    "sidewaystable",
    "sidewaysfigure",
];

/// Token positions of environment boundaries in `text` (outside comments), including `\[`/`\]`.
fn env_events(text: &str) -> Vec<(usize, usize, bool, String)> {
    let mut ev = Vec::new(); // (start, end, is_begin, name)
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let code = code_part(line);
        let b = code.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' {
                let rest = &code[i..];
                for (pfx, begin) in [("\\begin{", true), ("\\end{", false)] {
                    if rest.starts_with(pfx)
                        && let Some(close) = rest[pfx.len()..].find('}')
                    {
                        let name = rest[pfx.len()..pfx.len() + close].to_string();
                        ev.push((offset + i, offset + i + pfx.len() + close + 1, begin, name));
                    }
                }
                if rest.starts_with("\\[") {
                    ev.push((offset + i, offset + i + 2, true, "\\[".into()));
                } else if rest.starts_with("\\]") {
                    ev.push((offset + i, offset + i + 2, false, "\\[".into()));
                }
                i += 2;
                continue;
            }
            i += 1;
        }
        offset += line.len();
    }
    ev
}

/// Whether `pos` lies inside a `%` comment.
fn in_comment(text: &str, pos: usize) -> bool {
    let ls = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let le = text[pos..]
        .find('\n')
        .map(|i| pos + i)
        .unwrap_or(text.len());
    comment_start(&text[ls..le]).is_some_and(|c| ls + c <= pos)
}

/// Brace depth and number of `$` signs in `text`, ignoring comments and escaped characters.
fn balance(text: &str) -> (i32, u32) {
    let (mut depth, mut dollars) = (0i32, 0u32);
    for line in text.split_inclusive('\n') {
        let b = code_part(line).as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 1,
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b'$' => dollars += 1,
                _ => {}
            }
            i += 1;
        }
    }
    (depth, dollars)
}

/// From `pos`, moves to later line ends until the paragraph so far has balanced braces and
/// closed inline math, so the macro never lands inside `\cite{…}`, `$…$` or similar.
/// Gives up (returns `None`) at a paragraph break or after 64 lines.
fn settle(text: &str, para_start: usize, mut pos: usize) -> Option<usize> {
    for _ in 0..64 {
        let (depth, dollars) = balance(&text[para_start..pos]);
        if depth <= 0 && dollars % 2 == 0 {
            return Some(pos);
        }
        let next = text[pos..].find('\n').map(|i| pos + i + 1)?;
        let le = text[next..]
            .find('\n')
            .map(|i| next + i)
            .unwrap_or(text.len());
        if text[next..le].trim().is_empty() {
            return None;
        }
        pos = next + comment_start(&text[next..le]).unwrap_or(le - next);
    }
    None
}

/// Where to insert a comment about text ending at byte `end`: the end of its source line, moved
/// past any enclosing math/table/float/verbatim environment and outside braces and inline math.
/// Returns `None` when there is no safe spot (e.g. the quote is inside a `%` comment).
pub fn insertion_point(text: &str, end: usize) -> Option<usize> {
    if in_comment(text, end.saturating_sub(1)) {
        return None;
    }
    let le = text[end..]
        .find('\n')
        .map(|i| end + i)
        .unwrap_or(text.len());
    let ls = text[..end].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let mut pos = match comment_start(&text[ls..le]) {
        Some(c) => ls + c,
        None => le,
    };

    // Leave unsafe environments: find the outermost open one and jump past its \end.
    let events = env_events(text);
    let mut stack: Vec<(usize, &str)> = Vec::new();
    for (s, e, begin, name) in &events {
        if *s >= pos {
            break;
        }
        if *begin {
            stack.push((*e, name.as_str()));
        } else if let Some(k) = stack.iter().rposition(|(_, n)| n == name) {
            stack.truncate(k);
        }
    }
    if let Some(&(_, outer)) = stack
        .iter()
        .find(|(_, n)| *n == "\\[" || UNSAFE_ENVS.contains(n))
    {
        let mut depth = 0;
        let mut after = None;
        for (s, e, begin, name) in &events {
            if *s < pos || name != outer {
                continue;
            }
            if *begin {
                depth += 1;
            } else if depth == 0 {
                after = Some(*e);
                break;
            } else {
                depth -= 1;
            }
        }
        pos = after?;
    }

    let para_start = text[..pos].rfind("\n\n").map(|i| i + 2).unwrap_or(0);
    settle(text, para_start, pos)
}

/// Escapes plain text for use inside a LaTeX argument.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '{' | '}' | '$' | '&' | '#' | '_' | '%' => {
                out.push('\\');
                out.push(c);
            }
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            '<' => out.push_str("\\textless{}"),
            '>' => out.push_str("\\textgreater{}"),
            '|' => out.push_str("\\textbar{}"),
            '\n' | '\r' | '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The macro text for one AI comment.
pub fn comment_macro(macro_name: &str, label: &str, text: &str) -> String {
    let label: String = label
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | ',' | '-' | ':' | '&'))
        .collect();
    format!(
        " \\{macro_name}[AI: {}]{{{}}}",
        label.replace('&', "and"),
        escape(text)
    )
}

/// Removes every `\macro[AI…]{…}` from `text`. Returns the new text and how many were removed.
pub fn remove_ai_comments(text: &str, macro_name: &str) -> (String, usize) {
    let needle = format!("\\{macro_name}[AI");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut n = 0;
    while let Some(i) = rest.find(&needle) {
        let after = &rest[i + needle.len()..];
        let parsed = after.find(']').and_then(|close| {
            let tail = &after[close + 1..];
            let skip = tail.len() - tail.trim_start_matches([' ', '\t']).len();
            braced(&tail[skip..]).map(|(_, used)| needle.len() + close + 1 + skip + used)
        });
        match parsed {
            Some(len) => {
                let mut head = &rest[..i];
                if head.ends_with(' ') {
                    head = &head[..head.len() - 1];
                }
                out.push_str(head);
                rest = &rest[i + len..];
                n += 1;
            }
            None => {
                out.push_str(&rest[..i + needle.len()]);
                rest = &rest[i + needle.len()..];
            }
        }
    }
    out.push_str(rest);
    (out, n)
}

/// Adds `\usepackage{lightleaf-comments}` after `\documentclass` unless the package is loaded.
/// Returns `None` if nothing needs to change, or an error if there is no `\documentclass`.
pub fn ensure_package(main: &str) -> anyhow::Result<Option<String>> {
    if main
        .lines()
        .any(|l| code_part(l).contains(&format!("{{{PACKAGE}}}")))
    {
        return Ok(None);
    }
    let mut offset = 0;
    for line in main.split_inclusive('\n') {
        if let Some(i) = code_part(line).find("\\documentclass") {
            // Skip an optional argument that may span lines, then the mandatory one.
            let mut p = offset + i + "\\documentclass".len();
            let rest = main[p..].trim_start();
            p = main.len() - rest.len();
            if rest.starts_with('[') {
                p += rest
                    .find(']')
                    .ok_or_else(|| anyhow::anyhow!("unterminated \\documentclass options"))?
                    + 1;
            }
            let rest = main[p..].trim_start();
            p = main.len() - rest.len();
            let (_, used) =
                braced(rest).ok_or_else(|| anyhow::anyhow!("malformed \\documentclass"))?;
            p += used;
            let le = main[p..]
                .find('\n')
                .map(|k| p + k + 1)
                .unwrap_or(main.len());
            let mut out = String::with_capacity(main.len() + 120);
            out.push_str(&main[..le]);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!(
                "\\usepackage{{{PACKAGE}}} % review comments; [hide] hides them, [margin] puts them in the margin\n"
            ));
            out.push_str(&main[le..]);
            return Ok(Some(out));
        }
        offset += line.len();
    }
    anyhow::bail!("no \\documentclass found in the main file")
}

/// The `lightleaf-comments.sty` package, defining `\<macro_name>[label]{text}`.
pub fn package_source(macro_name: &str) -> String {
    include_str!("../tex/lightleaf-comments.sty").replace("@@MACRO@@", macro_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_quotes_exactly_and_across_whitespace() {
        let t = "We propose a new\n  method for fast   typesetting.";
        assert_eq!(find_quote(t, "propose a new"), Some((3, 16)));
        let (s, e) = find_quote(t, "new method for fast typesetting").unwrap();
        assert_eq!(&t[s..e], "new\n  method for fast   typesetting");
        assert_eq!(find_quote(t, "short"), None);
        assert_eq!(find_quote(t, "not in the text at all"), None);
    }

    #[test]
    fn inserts_at_line_end_and_before_comments() {
        let t = "First sentence here. Second one.\nNext line.";
        let (_, e) = find_quote(t, "First sentence here").unwrap();
        assert_eq!(insertion_point(t, e), Some(32));
        let t = "Some claim we make. % TODO cite\nMore.";
        let (_, e) = find_quote(t, "Some claim we make").unwrap();
        assert_eq!(&t[insertion_point(t, e).unwrap()..][..1], "%");
        let t = "% Some claim in a comment\nText.";
        assert_eq!(
            insertion_point(t, find_quote(t, "Some claim in a comment").unwrap().1),
            None
        );
    }

    #[test]
    fn leaves_math_tables_and_floats() {
        let t = "Text.\n\\begin{equation}\n  a = b + c \\label{eq}\n\\end{equation}\nAfter.";
        let (_, e) = find_quote(t, "a = b + c").unwrap();
        let p = insertion_point(t, e).unwrap();
        assert!(t[..p].ends_with("\\end{equation}"));
        let t = "\\begin{figure}\n\\begin{tabular}{c}\nA very long cell text \\\\\n\\end{tabular}\n\\caption{Cap}\n\\end{figure}\nX";
        let p = insertion_point(t, find_quote(t, "A very long cell text").unwrap().1).unwrap();
        assert!(t[..p].ends_with("\\end{figure}"));
        let t = "Display \\[ x = yyyyyyyyyy \\] done.";
        let p = insertion_point(t, find_quote(t, "x = yyyyyyyyyy").unwrap().1).unwrap();
        assert_eq!(p, t.len());
    }

    #[test]
    fn does_not_split_braces_or_inline_math() {
        let t = "As shown by \\citet{smith2020,\n  jones2021}, results hold.\nNext.";
        let p = insertion_point(t, find_quote(t, "As shown by").unwrap().1).unwrap();
        assert!(t[..p].ends_with("results hold."));
        let t = "The value $x +\n y$ is large enough.\nNext.";
        let p = insertion_point(t, find_quote(t, "The value").unwrap().1).unwrap();
        assert!(t[..p].ends_with("large enough."));
    }

    #[test]
    fn escapes_and_round_trips_comments() {
        let m = comment_macro("ANG", "Methods, warning", "Use 50% less & cite {x}_1 \\o/");
        assert_eq!(
            m,
            " \\ANG[AI: Methods, warning]{Use 50\\% less \\& cite \\{x\\}\\_1 \\textbackslash{}o/}"
        );
        let text = format!("A sentence.{m}\nB \\ANG{{human note}} c.");
        let (clean, n) = remove_ai_comments(&text, "ANG");
        assert_eq!(n, 1);
        assert_eq!(clean, "A sentence.\nB \\ANG{human note} c.");
    }

    #[test]
    fn adds_package_after_documentclass() {
        let t = "% header\n\\documentclass[11pt,\n  a4paper]{article}\n\\usepackage{amsmath}\n";
        let out = ensure_package(t).unwrap().unwrap();
        assert!(out.contains("a4paper]{article}\n\\usepackage{lightleaf-comments}"));
        assert!(ensure_package(&out).unwrap().is_none());
        assert!(ensure_package("no class").is_err());
    }

    #[test]
    fn splits_sections_and_appendix() {
        let t = "\\begin{abstract}Abs.\\end{abstract}\n\\section{Intro}\nI.\n\\section*{Related\n Work}\nR.\n\\appendix\n\\section[S]{Proofs}\nP.";
        let s = sections(t);
        let titles: Vec<_> = s.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(titles, ["Abstract", "Intro", "Related Work", "Proofs"]);
        assert!(s[3].in_appendix && !s[1].in_appendix);
        assert!(t[s[0].start..s[0].end].ends_with("\\end{abstract}"));
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
