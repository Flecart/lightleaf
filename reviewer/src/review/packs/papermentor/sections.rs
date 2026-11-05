//! Upstream's section model, reproduced exactly because the classifier and the reviewers'
//! inputs depend on it:
//! - the abstract is the first `\begin{abstract}…\end{abstract}` (level 0);
//! - headers are line-start `\section`/`\subsection`/`\subsubsection` (levels 1–3; titles end at
//!   the first `}`);
//! - a section runs from its header to the next header whose level ≤ max(own level, 2);
//! - sections after the first line-start `\appendix` are appendix sections.

#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub level: u8,
    pub in_appendix: bool,
    /// Byte range in the inlined text.
    pub start: usize,
    pub end: usize,
    pub content: String,
}

/// Byte offset of each line start.
fn line_starts(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.split_inclusive('\n').map(move |l| {
        let at = offset;
        offset += l.len();
        (at, l)
    })
}

fn header(line: &str) -> Option<(u8, String)> {
    for (cmd, level) in [
        ("\\subsubsection", 3u8),
        ("\\subsection", 2),
        ("\\section", 1),
    ] {
        if let Some(rest) = line.strip_prefix(cmd) {
            let rest = rest.strip_prefix('*').unwrap_or(rest);
            let rest = rest.strip_prefix('{')?;
            let end = rest.find('}')?;
            return Some((level, rest[..end].to_string()));
        }
    }
    None
}

pub fn parse(text: &str) -> Vec<Section> {
    let appendix_at = line_starts(text)
        .find(|(_, l)| {
            l.starts_with("\\appendix")
                && !l["\\appendix".len()..].starts_with(|c: char| c.is_alphanumeric())
        })
        .map(|(at, _)| at);
    let mut out = Vec::new();
    if let Some(b) = text.find("\\begin{abstract}") {
        let body_start = b + "\\begin{abstract}".len();
        if let Some(e) = text[body_start..].find("\\end{abstract}") {
            out.push(Section {
                title: "Abstract".into(),
                level: 0,
                in_appendix: false,
                start: b,
                end: body_start + e + "\\end{abstract}".len(),
                content: text[body_start..body_start + e].trim().to_string(),
            });
        }
    }
    let headers: Vec<(usize, u8, String)> = line_starts(text)
        .filter_map(|(at, l)| header(l).map(|(lvl, t)| (at, lvl, t)))
        .collect();
    for (k, (at, level, title)) in headers.iter().enumerate() {
        let bound = (*level).max(2);
        let end = headers[k + 1..]
            .iter()
            .find(|(_, l, _)| *l <= bound)
            .map(|(a, _, _)| *a)
            .unwrap_or(text.len());
        out.push(Section {
            title: title.clone(),
            level: *level,
            in_appendix: appendix_at.is_some_and(|a| *at > a),
            start: *at,
            end,
            content: text[*at..end].to_string(),
        });
    }
    out
}

/// The figure and table environments with three lines of context on each side, joined with
/// `\n\n---\n\n` (the Figures & Captions reviewer's input).
pub fn figures_and_tables(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let env = ["figure*", "figure", "table*", "table"]
            .into_iter()
            .find(|e| l.contains(&format!("\\begin{{{e}}}")));
        if let Some(env) = env {
            let end_tag = format!("\\end{{{env}}}");
            let end = (i..lines.len())
                .find(|&j| lines[j].contains(&end_tag))
                .unwrap_or(lines.len() - 1);
            let from = i.saturating_sub(3);
            let to = (end + 3).min(lines.len() - 1);
            blocks.push(lines[from..=to].join("\n"));
            i = end + 1;
        } else {
            i += 1;
        }
    }
    blocks.join("\n\n---\n\n")
}

/// The content of the sections with the given titles (exact, case-insensitive; if none
/// matches exactly, substring in either direction), without sections nested in another
/// matched section, joined with blank lines.
pub fn collect(sections: &[Section], titles: &[String]) -> String {
    let want: Vec<String> = titles.iter().map(|t| t.trim().to_lowercase()).collect();
    let title = |s: &Section| s.title.trim().to_lowercase();
    let mut matched: Vec<&Section> = sections
        .iter()
        .filter(|s| want.contains(&title(s)))
        .collect();
    if matched.is_empty() {
        matched = sections
            .iter()
            .filter(|s| {
                want.iter().any(|w| {
                    !w.is_empty() && (title(s).contains(w.as_str()) || w.contains(&title(s)))
                })
            })
            .collect();
    }
    let outer: Vec<&Section> = matched
        .iter()
        .filter(|s| {
            !matched.iter().any(|o| {
                !std::ptr::eq(*o, **s)
                    && o.start <= s.start
                    && s.end <= o.end
                    && (o.start, o.end) != (s.start, s.end)
            })
        })
        .copied()
        .collect();
    outer
        .iter()
        .map(|s| s.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAPER: &str = "\\begin{abstract}\nWe do X.\n\\end{abstract}\n\\section{Introduction}\nIntro.\n\\subsection{Motivation}\nWhy.\n\\subsubsection{Detail}\nD.\n\\section{Method}\nM.\n\\appendix\n\\section{Proofs}\nP.\n";

    #[test]
    fn parses_levels_ranges_and_appendix() {
        let s = parse(PAPER);
        let t: Vec<(&str, u8, bool)> = s
            .iter()
            .map(|x| (x.title.as_str(), x.level, x.in_appendix))
            .collect();
        assert_eq!(
            t,
            [
                ("Abstract", 0, false),
                ("Introduction", 1, false),
                ("Motivation", 2, false),
                ("Detail", 3, false),
                ("Method", 1, false),
                ("Proofs", 1, true)
            ]
        );
        assert_eq!(s[0].content, "We do X.");
        // A \section holds only its text before the first \subsection; \subsubsection stays in its parent.
        assert_eq!(s[1].content, "\\section{Introduction}\nIntro.\n");
        assert!(s[2].content.contains("Detail") && s[2].content.contains("D."));
    }

    #[test]
    fn collects_without_nested_duplicates() {
        let s = parse(PAPER);
        let c = collect(&s, &["Motivation".into(), "detail".into()]);
        assert_eq!(c.matches("D.").count(), 1);
    }

    #[test]
    fn extracts_figures_with_context() {
        let t = "a\nb\nc\nd\n\\begin{figure}\nX\n\\caption{C}\n\\end{figure}\ne\nf\ng\nh";
        let f = figures_and_tables(t);
        assert!(f.starts_with("b\nc\nd\n\\begin{figure}") && f.ends_with("\\end{figure}\ne\nf\ng"));
    }
}
