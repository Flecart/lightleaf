//! The comment macro (`\ANG[AI: label]{text}` by default), its removal, and the
//! `lightleaf-comments` package that defines it.

use super::{PACKAGE, braced, code_part};

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
pub fn remove_comments(text: &str, macro_name: &str) -> (String, usize) {
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
    include_str!("../../tex/lightleaf-comments.sty").replace("@@MACRO@@", macro_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_round_trips_comments() {
        let m = comment_macro("ANG", "Methods, warning", "Use 50% less & cite {x}_1 \\o/");
        assert_eq!(
            m,
            " \\ANG[AI: Methods, warning]{Use 50\\% less \\& cite \\{x\\}\\_1 \\textbackslash{}o/}"
        );
        let text = format!("A sentence.{m}\nB \\ANG{{human note}} c.");
        let (clean, n) = remove_comments(&text, "ANG");
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
}
