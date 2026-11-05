//! Upstream's two rule-based checks (no model call):
//! - Abstract Footnote Check: the abstract should end with a footnote linking code and data;
//! - Section Structure Check: an experimental setup section should exist and precede results.

use super::sections::Section;
use crate::review::{BoxFuture, Finding, Reviewer, Services, Severity, template::render};
use std::sync::Arc;

/// A reviewer whose findings are computed while planning.
pub struct RuleReviewer {
    pub id: &'static str,
    pub name: &'static str,
    pub findings: Vec<Finding>,
}

impl Reviewer for RuleReviewer {
    fn id(&self) -> &str {
        self.id
    }
    fn name(&self) -> &str {
        self.name
    }
    fn max_findings(&self) -> usize {
        1
    }
    fn review(
        self: Arc<Self>,
        _: Arc<Services>,
    ) -> BoxFuture<'static, anyhow::Result<Vec<Finding>>> {
        Box::pin(async move { Ok(self.findings.clone()) })
    }
}

/// Unescaped `%` before `pos` on the same line.
fn commented(text: &str, pos: usize) -> bool {
    let ls = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &text[ls..pos];
    let b = line.as_bytes();
    (0..b.len()).any(|i| b[i] == b'%' && (i == 0 || b[i - 1] != b'\\'))
}

/// Position of the last sentence break (`[.!?]` + whitespace + capital or `\`) in `s`: the
/// byte offset right after it.
fn sentence_breaks(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    for i in 0..b.len() {
        if matches!(b[i], b'.' | b'!' | b'?') {
            let mut j = i + 1;
            while j < b.len() && (b[j] as char).is_whitespace() {
                j += 1;
            }
            if j > i + 1 && j < b.len() && (b[j].is_ascii_uppercase() || b[j] == b'\\') {
                out.push(j);
            }
        }
    }
    out
}

/// The abstract-footnote finding, if the abstract's last sentence has no footnote.
pub fn abstract_footnote(sections: &[Section], comment: &str) -> Option<Finding> {
    let abs = sections.iter().find(|s| s.level == 0)?;
    // Drop trailing blank and comment-only lines.
    let mut lines: Vec<&str> = abs.content.lines().collect();
    while lines.last().is_some_and(|l| {
        let t = l.trim();
        t.is_empty() || t.starts_with('%')
    }) {
        lines.pop();
    }
    let body = lines.join("\n").trim_end().to_string();
    if body.is_empty() {
        return None;
    }
    // Last uncommented \footnote / \footnotemark / \thanks.
    let mut last = None;
    for cmd in ["\\footnote", "\\footnotemark", "\\thanks"] {
        let mut from = 0;
        while let Some(i) = body[from..].find(cmd) {
            let at = from + i;
            let after = at + cmd.len();
            let word_end = !body[after..].starts_with(|c: char| c.is_ascii_alphabetic());
            if word_end && !commented(&body, at) && last.is_none_or(|(l, _)| at > l) {
                last = Some((at, after));
            }
            from = after;
        }
    }
    if let Some((_, mut after)) = last {
        // Skip an optional [..] and the braced argument.
        if body[after..].starts_with('[')
            && let Some(close) = body[after..].find(']')
        {
            after += close + 1;
        }
        if body[after..].starts_with('{') {
            let b = body.as_bytes();
            let mut depth = 0;
            let mut i = after;
            while i < b.len() {
                match b[i] {
                    b'\\' => i += 1,
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            after = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        if sentence_breaks(&body[after..]).is_empty() {
            return None; // the footnote is in the last sentence
        }
    }
    let start = sentence_breaks(&body).last().copied().unwrap_or(0);
    let mut last_sentence = body[start..].to_string();
    // Remove a trailing inline comment.
    if let Some(p) = (0..last_sentence.len()).find(|&i| {
        last_sentence.as_bytes()[i] == b'%' && (i == 0 || last_sentence.as_bytes()[i - 1] != b'\\')
    }) {
        last_sentence.truncate(p);
    }
    let mut quote = last_sentence.trim().to_string();
    if quote.chars().count() > 200 {
        let tail: String = quote
            .chars()
            .rev()
            .take(200)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        quote = tail
            .split_once(' ')
            .map(|(_, r)| r.to_string())
            .unwrap_or(tail);
    }
    (!quote.is_empty()).then(|| Finding {
        quote,
        text: comment.trim().to_string(),
        severity: Severity::Warning,
    })
}

/// The role of a section from its title: setup, results, or neither.
fn role(title: &str) -> Option<&'static str> {
    let mut t = String::new();
    let mut chars = title.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == '*')
            {
                chars.next();
            }
            t.push(' ');
        } else if c == '{' || c == '}' {
            t.push(' ');
        } else {
            t.push(c);
        }
    }
    let t = format!(" {} ", t.trim().to_lowercase());
    let word = |w: &str| {
        let bytes = t.as_bytes();
        t.match_indices(w).any(|(i, m)| {
            let before = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
            let after = i + m.len() >= bytes.len() || !bytes[i + m.len()].is_ascii_alphanumeric();
            before && after
        })
    };
    if ["problem", "task", "theoretical", "theory", "formal", "game"]
        .iter()
        .any(|w| word(w))
    {
        return None;
    }
    let setup = [
        "setup",
        "setups",
        "set-up",
        "set-ups",
        "set up",
        "settings",
        "setting",
        "implementation detail",
        "implementation details",
        "training detail",
        "training details",
        "evaluation protocol",
        "evaluation details",
        "evaluation metric",
        "evaluation metrics",
        "experimental detail",
        "experimental details",
        "dataset and baselines",
        "datasets and baselines",
        "datasets and metrics",
        "dataset and metrics",
        "datasets and models",
        "dataset and models",
        "baselines and metrics",
        "baseline and metrics",
    ];
    if setup.iter().any(|w| word(w)) {
        return Some("setup");
    }
    let results = [
        "experiment",
        "experiments",
        "result",
        "results",
        "evaluation",
        "evaluations",
        "analysis",
        "analyses",
        "findings",
        "ablation",
        "ablations",
    ];
    if results.iter().any(|w| word(w))
        || t.trim_start().starts_with("rq")
            && t.trim_start()[2..]
                .trim_start()
                .starts_with(|c: char| c.is_ascii_digit())
    {
        return Some("results");
    }
    None
}

/// Prose left after removing headers, comments and non-prose environments.
fn has_prose(content: &str) -> bool {
    let mut out = String::new();
    let mut skip_env: Option<String> = None;
    for line in content.lines() {
        let l = line.trim();
        if let Some(env) = &skip_env {
            if l.contains(&format!("\\end{{{env}}}")) {
                skip_env = None;
            }
            continue;
        }
        if let Some(e) = [
            "figure",
            "table",
            "equation",
            "align",
            "gather",
            "multline",
            "eqnarray",
            "algorithm",
            "tabular",
            "lstlisting",
            "verbatim",
            "minted",
            "tikzpicture",
        ]
        .iter()
        .find(|e| l.starts_with(&format!("\\begin{{{e}")))
        {
            let name = l[7..].split('}').next().unwrap_or(e).to_string();
            if !l.contains(&format!("\\end{{{name}}}")) {
                skip_env = Some(name);
            }
            continue;
        }
        if l.starts_with("\\section")
            || l.starts_with("\\subsection")
            || l.starts_with("\\subsubsection")
            || l.starts_with("\\label")
            || l.starts_with('%')
        {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out.split("\n\n").any(|p| p.trim().len() >= 40)
}

/// The section-structure finding, if setup is missing or comes after the first results.
pub fn section_structure(
    sections: &[Section],
    no_setup: &str,
    setup_after: &str,
) -> Option<Finding> {
    // Labels: level-2 sections are "<parent> > <title>".
    let mut parent = String::new();
    let entries: Vec<(String, &Section)> = sections
        .iter()
        .filter(|s| !s.in_appendix && (1..=2).contains(&s.level))
        .map(|s| {
            if s.level == 1 {
                parent = s.title.clone();
                (s.title.clone(), s)
            } else {
                (format!("{parent} > {}", s.title), s)
            }
        })
        .collect();
    let setups: Vec<&(String, &Section)> = entries
        .iter()
        .filter(|(_, s)| role(&s.title) == Some("setup"))
        .collect();
    let first = entries.iter().find(|(label, s)| {
        role(&s.title) == Some("results")
            && has_prose(&s.content)
            && !setups
                .iter()
                .any(|(l, _)| l.starts_with(&format!("{label} > ")))
    })?;
    let header = |s: &Section| s.content.lines().next().unwrap_or("").trim().to_string();
    let top_label = first.0.split(" > ").next().unwrap_or(&first.0).to_string();
    if setups.is_empty() {
        let top = entries
            .iter()
            .find(|(l, _)| *l == top_label)
            .map(|(_, s)| *s)
            .unwrap_or(first.1);
        let text = render(
            no_setup,
            &[("topLabel", &top_label), ("firstResultsLabel", &first.0)],
        );
        return Some(Finding {
            quote: header(top),
            text: text.trim().into(),
            severity: Severity::Warning,
        });
    }
    let setup = setups[0];
    if setup.1.start > first.1.start {
        let text = render(
            setup_after,
            &[("setupLabel", &setup.0), ("firstResultsLabel", &first.0)],
        );
        return Some(Finding {
            quote: header(setup.1),
            text: text.trim().into(),
            severity: Severity::Warning,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::packs::papermentor::sections::parse;

    const PROSE: &str = "This paragraph is long enough to count as prose in the section text.";

    #[test]
    fn abstract_footnote_check() {
        let with = parse(
            "\\begin{abstract}\nWe study X. Our code is public.\\footnote{\\url{https://x}}\n\\end{abstract}",
        );
        assert!(abstract_footnote(&with, "c").is_none());
        let without = parse(
            "\\begin{abstract}\nWe study X. We find that Y holds in all cases.\n% note\n\\end{abstract}",
        );
        let f = abstract_footnote(&without, "c").unwrap();
        assert_eq!(f.quote, "We find that Y holds in all cases.");
        // As upstream: text after the footnote with no further sentence break still counts as
        // the last sentence.
        let one_after =
            parse("\\begin{abstract}\nWe study X.\\footnote{a} We find Y.\n\\end{abstract}");
        assert!(abstract_footnote(&one_after, "c").is_none());
        let early = parse(
            "\\begin{abstract}\nWe study X.\\footnote{a} We find Y. We conclude Z.\n\\end{abstract}",
        );
        assert_eq!(
            abstract_footnote(&early, "c").unwrap().quote,
            "We conclude Z."
        );
    }

    #[test]
    fn section_structure_check() {
        let missing = parse(&format!(
            "\\section{{Experiments}}\n{PROSE}\n\\section{{Conclusion}}\nC.\n"
        ));
        let f =
            section_structure(&missing, "no {{topLabel}} {{firstResultsLabel}}", "after").unwrap();
        assert_eq!(
            (f.quote.as_str(), f.text.as_str()),
            ("\\section{Experiments}", "no Experiments Experiments")
        );
        let late = parse(&format!(
            "\\section{{Results}}\n{PROSE}\n\\section{{Experimental Setup}}\n{PROSE}\n"
        ));
        let f =
            section_structure(&late, "no", "{{setupLabel}} after {{firstResultsLabel}}").unwrap();
        assert_eq!(f.text, "Experimental Setup after Results");
        let good = parse(&format!(
            "\\section{{Experiments}}\n\\subsection{{Setup}}\n{PROSE}\n\\subsection{{Main Results}}\n{PROSE}\n"
        ));
        assert!(section_structure(&good, "no", "after").is_none());
        assert_eq!(role("Problem Formulation and Results"), None);
        assert_eq!(role("RQ1: Does it work?"), Some("results"));
    }
}
