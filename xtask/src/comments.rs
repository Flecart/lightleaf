//! The comment rule: plain (non-doc) comments are refused unless they carry an allowed marker.
//!
//! Comments are found with `ra_ap_rustc_lexer`, the lexer rustc itself uses, so raw strings
//! holding `//`, lifetimes next to char literals and nested block comments are classified the
//! way the compiler sees them. A regular expression gets all three wrong.
//!
//! Always allowed: doc comments (`///`, `//!`, `/** */`, `/*! */`) and plain comments whose text
//! starts with one of [`MARKERS`] or `TODO(name):`. In a block of consecutive own-line `//`
//! comments, a marker line covers itself and the rest of the block.

use ra_ap_rustc_lexer::{FrontmatterAllowed, TokenKind, strip_shebang, tokenize};

/// Comment prefixes that are always allowed (after `//` or `/*` and optional spaces).
pub const MARKERS: &[&str] = &[
    "SAFETY:",
    "keep:",
    "@generated",
    "SPDX-License-Identifier:",
    "SPDX-FileCopyrightText:",
    "Copyright",
];

/// One plain (non-doc) comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Byte range in the source, from `//` or `/*` to the end of the comment (newline excluded).
    pub start: usize,
    /// See `start`.
    pub end: usize,
    /// 1-based line of `start`.
    pub line: usize,
    /// Only whitespace before it on its first line and after it on its last line.
    pub own_line: bool,
    /// The comment text, delimiters included.
    pub text: String,
    /// Whether the rule allows it.
    pub allowed: bool,
}

/// Whether a comment's text (delimiters included) starts with an allowed marker.
fn has_marker(text: &str) -> bool {
    let body = text
        .strip_prefix("//")
        .or_else(|| text.strip_prefix("/*"))
        .unwrap_or(text)
        .trim_start();
    MARKERS.iter().any(|m| body.starts_with(m)) || is_todo(body)
}

/// `TODO(name):` with a non-empty name.
fn is_todo(body: &str) -> bool {
    body.strip_prefix("TODO(")
        .and_then(|rest| rest.split_once("):"))
        .is_some_and(|(name, _)| !name.trim().is_empty() && !name.contains(['(', ')']))
}

/// Every plain comment in `src`, classified.
pub fn scan(src: &str) -> Vec<Comment> {
    let offset = strip_shebang(src).unwrap_or(0);
    let mut out: Vec<Comment> = Vec::new();
    let mut pos = offset;
    for token in tokenize(&src[offset..], FrontmatterAllowed::Yes) {
        let start = pos;
        pos += token.len as usize;
        let (TokenKind::LineComment { doc_style: None }
        | TokenKind::BlockComment {
            doc_style: None, ..
        }) = token.kind
        else {
            continue;
        };
        let end = pos;
        let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = src[end..].find('\n').map_or(src.len(), |i| end + i);
        out.push(Comment {
            start,
            end,
            line: src[..start].matches('\n').count() + 1,
            own_line: src[line_start..start].trim().is_empty()
                && src[end..line_end].trim().is_empty(),
            text: src[start..end].to_string(),
            allowed: false,
        });
    }
    classify(src, &mut out);
    out
}

/// Applies the marker rule, letting a marker line cover the rest of its `//` block.
fn classify(src: &str, comments: &mut [Comment]) {
    let mut covered = false;
    for i in 0..comments.len() {
        let continues_block = i > 0 && {
            let (prev, cur) = (&comments[i - 1], &comments[i]);
            is_block_line(prev)
                && is_block_line(cur)
                && cur.line == prev.line + 1
                && src[prev.end..cur.start].trim().is_empty()
        };
        if !continues_block {
            covered = false;
        }
        if has_marker(&comments[i].text) && (is_block_line(&comments[i]) || !continues_block) {
            covered = is_block_line(&comments[i]);
            comments[i].allowed = true;
        } else {
            comments[i].allowed = covered;
        }
    }
}

/// An own-line `//` comment, the kind that forms blocks.
fn is_block_line(c: &Comment) -> bool {
    c.own_line && c.text.starts_with("//")
}

/// `src` without the refused comments: own-line comments lose their whole lines, trailing ones
/// are cut with the spaces before them, and the blank lines a deletion leaves behind are
/// collapsed the way rustfmt would (no double blank lines, none after `{` or before `}`).
/// Returns `None` when nothing is refused.
pub fn strip(src: &str, comments: &[Comment]) -> Option<String> {
    let refused: Vec<&Comment> = comments.iter().filter(|c| !c.allowed).collect();
    if refused.is_empty() {
        return None;
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut at = 0;
    for raw in src.split_inclusive('\n') {
        lines.push(Line {
            start: at,
            text: raw.to_string(),
            removed: false,
        });
        at += raw.len();
    }
    let starts: Vec<usize> = lines.iter().map(|l| l.start).collect();
    let line_of = |byte: usize| starts.partition_point(|&s| s <= byte) - 1;
    for c in refused.iter().rev() {
        let (first, last) = (line_of(c.start), line_of(c.end.saturating_sub(1)));
        if c.own_line {
            for l in &mut lines[first..=last] {
                l.removed = true;
            }
            continue;
        }
        let (fl, ll) = (&lines[first], &lines[last]);
        let before = &fl.text[..c.start - fl.start];
        let after = &ll.text[c.end - ll.start..];
        let joined = if after.trim().is_empty() {
            format!("{}{}", before.trim_end(), newline_of(after))
        } else if before.ends_with(char::is_whitespace) {
            format!("{before}{}", after.trim_start())
        } else {
            format!("{before}{after}")
        };
        lines[first].text = joined;
        for l in &mut lines[first + 1..=last] {
            l.removed = true;
        }
    }
    tidy_seams(&mut lines);
    Some(
        lines
            .iter()
            .filter(|l| !l.removed)
            .map(|l| l.text.as_str())
            .collect(),
    )
}

/// The line ending at the end of `s` (`"\n"`, `"\r\n"` or none).
fn newline_of(s: &str) -> &str {
    if s.ends_with("\r\n") {
        "\r\n"
    } else if s.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

struct Line {
    start: usize,
    text: String,
    removed: bool,
}

/// Collapses blank lines around each run of removed lines.
fn tidy_seams(lines: &mut [Line]) {
    let blank = |l: &Line| l.text.trim().is_empty();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].removed {
            i += 1;
            continue;
        }
        let run_end = (i..lines.len())
            .find(|&j| !lines[j].removed)
            .unwrap_or(lines.len());
        let prev = (0..i).rev().find(|&j| !lines[j].removed);
        let next = (run_end < lines.len()).then_some(run_end);
        let blank_before_close = next.is_some_and(|n| {
            blank(&lines[n])
                && (n + 1..lines.len())
                    .find(|&j| !lines[j].removed)
                    .is_some_and(|j| lines[j].text.trim_start().starts_with('}'))
        });
        match (prev, next) {
            (Some(_), Some(n)) if blank_before_close => lines[n].removed = true,
            (Some(p), Some(n)) if blank(&lines[p]) && blank(&lines[n]) => lines[n].removed = true,
            (Some(p), Some(n)) if lines[p].text.trim_end().ends_with('{') && blank(&lines[n]) => {
                lines[n].removed = true;
            }
            (Some(p), Some(n))
                if blank(&lines[p]) && lines[n].text.trim_start().starts_with('}') =>
            {
                lines[p].removed = true;
            }
            (None, Some(n)) if blank(&lines[n]) => lines[n].removed = true,
            (Some(p), None) if blank(&lines[p]) => lines[p].removed = true,
            _ => {}
        }
        i = run_end;
    }
}

/// Whether two sources have the same code: equal token streams of their `syn` parse.
/// Plain comments are not tokens, so equality means only comments and whitespace differ.
/// `Err` when either does not parse.
pub fn same_code(old: &str, new: &str) -> Result<bool, String> {
    use quote::ToTokens;
    let tokens = |s: &str| {
        syn::parse_file(s)
            .map(|f| f.to_token_stream().to_string())
            .map_err(|e| e.to_string())
    };
    Ok(tokens(old)? == tokens(new)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r##"//! Module docs stay.

use std::fmt;

/// Item docs stay.
fn f<'a>(s: &'a str) -> char {
    // narration that goes
    let url = r#"https://example.com // not a comment"#;
    let c = 'x'; // trailing narration
    /* outer /* nested */ still the same comment */
    // SAFETY: nothing unsafe here, the marker is what counts
    // keep: the server rejects lower-case names,
    // so this stays upper case.
    let _ = (url, s, fmt::Error);
    c
}
"##;

    fn texts(src: &str, allowed: bool) -> Vec<String> {
        scan(src)
            .into_iter()
            .filter(|c| c.allowed == allowed)
            .map(|c| c.text)
            .collect()
    }

    #[test]
    fn finds_plain_comments_only() {
        assert_eq!(
            texts(SAMPLE, false),
            [
                "// narration that goes",
                "// trailing narration",
                "/* outer /* nested */ still the same comment */",
            ]
        );
    }

    #[test]
    fn markers_cover_their_block() {
        assert_eq!(
            texts(SAMPLE, true),
            [
                "// SAFETY: nothing unsafe here, the marker is what counts",
                "// keep: the server rejects lower-case names,",
                "// so this stays upper case.",
            ]
        );
    }

    #[test]
    fn a_block_without_a_marker_on_its_first_line_is_refused() {
        let src = "fn f() {\n    // first\n    // second\n}\n";
        assert_eq!(texts(src, false), ["// first", "// second"]);
    }

    #[test]
    fn todo_needs_a_name() {
        assert!(has_marker("// TODO(ana): later"));
        assert!(!has_marker("// TODO: later"));
        assert!(!has_marker("// TODO(): later"));
    }

    #[test]
    fn strip_keeps_code_and_allowed_comments() {
        let new = strip(SAMPLE, &scan(SAMPLE)).expect("something to strip");
        assert!(same_code(SAMPLE, &new).unwrap());
        assert!(new.contains(r##"r#"https://example.com // not a comment"#"##));
        assert!(new.contains("    let c = 'x';\n"));
        assert!(new.contains("// SAFETY:") && new.contains("// so this stays upper case."));
        assert!(!new.contains("narration") && !new.contains("nested"));
        assert!(new.contains("fn f<'a>(s: &'a str) -> char {\n    let url"));
    }

    #[test]
    fn strip_collapses_blank_lines_like_rustfmt() {
        let src =
            "fn f() {\n    let a = 1;\n\n    // gone\n\n    let b = a;\n    // also gone\n\n}\n";
        let new = strip(src, &scan(src)).unwrap();
        assert_eq!(new, "fn f() {\n    let a = 1;\n\n    let b = a;\n}\n");
    }

    #[test]
    fn nothing_to_strip_is_none() {
        let src = "/// docs\nfn f() {} // keep: load-bearing\n";
        assert_eq!(strip(src, &scan(src)), None);
    }

    #[test]
    fn same_code_sees_code_changes() {
        assert!(!same_code("fn f() {}", "fn g() {}").unwrap());
        assert!(same_code("fn f() {} // x", "fn f() {}").unwrap());
    }
}
