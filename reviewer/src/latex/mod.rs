//! LaTeX source handling for the reviewer.
//!
//! - [`document`]: collect the main file and everything it `\input`s.
//! - [`sections`]: split the document into sections (abstract, chapters, sections, appendix).
//! - [`anchor`]: find a quoted passage and a position after it where a macro is safe.
//! - [`markup`]: the comment macro, its removal, and the `lightleaf-comments` package.
//! - [`annotate`]: apply comments to (or remove them from) a checkout on disk.

pub mod anchor;
pub mod annotate;
pub mod document;
pub mod markup;
pub mod sections;

pub use anchor::find_quote;
pub use document::{Document, gather};
pub use sections::sections;

/// Name of the package the reviewer adds to projects (`lightleaf-comments.sty`).
pub const PACKAGE: &str = "lightleaf-comments";

/// Index of the first unescaped `%` in `line`, if any.
pub(crate) fn comment_start(line: &str) -> Option<usize> {
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
pub(crate) fn code_part(line: &str) -> &str {
    &line[..comment_start(line).unwrap_or(line.len())]
}

/// Reads a balanced `{…}` group starting at `s[0] == '{'`; returns (content, bytes consumed).
pub(crate) fn braced(s: &str) -> Option<(&str, usize)> {
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
