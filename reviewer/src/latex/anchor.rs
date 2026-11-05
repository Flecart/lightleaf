//! Anchoring: finding a quoted passage in a source file, and choosing a position after it
//! where a comment macro cannot break compilation.

use super::{code_part, comment_start};

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
}
