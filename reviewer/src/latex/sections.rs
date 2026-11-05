//! Splitting a document into its abstract and top-level sections.

use super::{braced, code_part};

/// A section of the merged document, as given to the classifier.
#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub in_appendix: bool,
    /// Byte range in the merged text.
    pub start: usize,
    pub end: usize,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sections_and_appendix() {
        let t = "\\begin{abstract}Abs.\\end{abstract}\n\\section{Intro}\nI.\n\\section*{Related\n Work}\nR.\n\\appendix\n\\section[S]{Proofs}\nP.";
        let s = sections(t);
        let titles: Vec<_> = s.iter().map(|x| x.title.as_str()).collect();
        assert_eq!(titles, ["Abstract", "Intro", "Related Work", "Proofs"]);
        assert!(s[3].in_appendix && !s[1].in_appendix);
        assert!(t[s[0].start..s[0].end].ends_with("\\end{abstract}"));
    }
}
