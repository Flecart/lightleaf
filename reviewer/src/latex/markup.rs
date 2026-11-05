//! The comment macro (`\AI[label]{text}` by default), its removal, and the
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

/// The macro text for one comment: ` \<macro>[<label>]{<escaped text>}` (leading space included).
pub fn comment_macro(macro_name: &str, label: &str, text: &str) -> String {
    let label: String = label
        .replace('&', "and")
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | ',' | '-' | ':'))
        .collect();
    format!(" \\{macro_name}[{label}]{{{}}}", escape(text))
}

/// Length of a `\<macro>[…]{…}` or `\<macro>{…}` occurrence at the start of `s`, if well formed.
fn macro_len(s: &str, macro_name: &str) -> Option<usize> {
    let mut i = macro_name.len() + 1;
    // `\AI` must not be the prefix of a longer command such as `\AIbox`.
    if s[i..].starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let rest = &s[i..];
    let trimmed = rest.trim_start_matches([' ', '\t']);
    i += rest.len() - trimmed.len();
    if trimmed.starts_with('[') {
        i += trimmed.find(']')? + 1;
        let rest = &s[i..];
        i += rest.len() - rest.trim_start_matches([' ', '\t']).len();
    }
    braced(&s[i..]).map(|(_, used)| i + used)
}

/// Removes every `\<macro>[…]{…}` and `\<macro>{…}` (and the space before it) from `text`.
/// The macro is reserved for review comments, so all of them are removed.
/// Returns the new text and how many were removed.
pub fn remove_comments(text: &str, macro_name: &str) -> (String, usize) {
    let needle = format!("\\{macro_name}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut n = 0;
    while let Some(i) = rest.find(&needle) {
        match macro_len(&rest[i..], macro_name) {
            Some(len) => {
                out.push_str(rest[..i].strip_suffix(' ').unwrap_or(&rest[..i]));
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
        let m = comment_macro("AI", "Methods, warning", "Use 50% less & cite {x}_1 \\o/");
        assert_eq!(
            m,
            " \\AI[Methods, warning]{Use 50\\% less \\& cite \\{x\\}\\_1 \\textbackslash{}o/}"
        );
        let text = format!("A sentence.{m}\nB \\AI{{plain}} c \\AIbox{{keep}} \\ANG{{human}}.");
        let (clean, n) = remove_comments(&text, "AI");
        assert_eq!(n, 2);
        assert_eq!(clean, "A sentence.\nB c \\AIbox{keep} \\ANG{human}.");
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
