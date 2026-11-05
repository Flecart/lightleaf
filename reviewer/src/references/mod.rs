//! The reference checker: verifies the bibliography entries a paper cites against Semantic
//! Scholar, and flags references that look fabricated, point to another paper, or disagree on
//! authors, year, venue or DOI. A port of upstream `CitationVerifier` ("Citation Sleuth"), with
//! its verdicts, thresholds and messages, and with these changes:
//! - comments are anchored at the `\cite` in the paper (a macro inside a .bib entry would
//!   break BibTeX), so they appear in the PDF next to the citation;
//! - only entries the paper cites are checked; cited keys missing from every .bib and keys
//!   defined twice are reported too;
//! - an invalid API key stops the check instead of flagging every entry as fabricated;
//! - the cache keeps lookups (not verdicts) and expires, so fixing an entry re-evaluates it;
//! - DOI prefixes are normalised and LaTeX accents decoded before comparing.

mod bib;
mod cites;
mod normalize;
pub mod s2;
mod verify;

use crate::{latex::Document, review::Severity};
use normalize::{normalize_doi, strip_latex};
use s2::{Client, Lookup};
use serde::Serialize;
use std::{collections::HashMap, path::Path};

/// One reportable problem, anchored at the first citation of the key (if cited).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    pub key: String,
    pub verdict: String,
    pub severity: Severity,
    pub message: String,
    pub bib_file: Option<String>,
    /// Where it is cited: (tex file, `\cite…{…}` text).
    #[serde(skip)]
    pub cited_at: Option<(String, String)>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub bib_files: Vec<String>,
    pub cited: usize,
    pub checked: usize,
    pub by_verdict: std::collections::BTreeMap<String, usize>,
    pub problems: Vec<Problem>,
    /// Entries defined but never cited (informational).
    pub uncited: usize,
    pub truncated: bool,
}

/// The project's .bib files: those named by `\bibliography`/`\addbibresource`, else all.
fn find_bib_files(dir: &Path, doc: &Document) -> Vec<String> {
    let named: Vec<String> = cites::bib_files(doc)
        .into_iter()
        .filter(|p| dir.join(p).is_file())
        .collect();
    if !named.is_empty() {
        return named;
    }
    let mut all = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if e.file_name() == ".git" || e.file_type().is_ok_and(|t| t.is_symlink()) {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "bib")
                && let Ok(rel) = p.strip_prefix(dir)
            {
                all.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    all.sort();
    all
}

pub async fn check(
    dir: &Path,
    doc: &Document,
    client: &Client,
    max_entries: usize,
    progress: &(dyn Fn(String) + Send + Sync),
) -> anyhow::Result<Report> {
    let mut report = Report {
        bib_files: find_bib_files(dir, doc),
        ..Default::default()
    };
    let mut entries: HashMap<String, bib::Entry> = HashMap::new();
    let mut problems = Vec::new();
    for f in &report.bib_files {
        let text = std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        for e in bib::parse(&text, f) {
            if let Some(prev) = entries.get(&e.key) {
                problems.push(Problem {
                    key: e.key.clone(),
                    verdict: "duplicate_key".into(),
                    severity: Severity::Warning,
                    message: format!("The key \"{}\" is defined more than once ({} and {}); BibTeX uses only one of them.", e.key, prev.file, e.file),
                    bib_file: Some(e.file.clone()),
                    cited_at: None,
                });
                continue;
            }
            entries.insert(e.key.clone(), e);
        }
    }
    let cited = cites::citations(doc);
    report.cited = cited.len();
    report.uncited = entries.keys().filter(|k| !cited.contains_key(*k)).count();
    for (key, c) in &cited {
        if !entries.contains_key(key) && !report.bib_files.is_empty() {
            problems.push(Problem {
                key: key.clone(),
                verdict: "undefined_key".into(),
                severity: Severity::Critical,
                message: format!("\"{key}\" is cited but not defined in any .bib file ({}); it will print as \"?\".", report.bib_files.join(", ")),
                bib_file: None,
                cited_at: Some((c.file.clone(), c.command.clone())),
            });
        }
    }
    // Only cited entries are verified, in citation order, up to the cap. Web pages and software
    // are not in Semantic Scholar, so without a DOI they are not looked up.
    let mut todo: Vec<&bib::Entry> = cited
        .keys()
        .filter_map(|k| entries.get(k))
        .filter(|e| {
            let web = matches!(
                e.kind.as_str(),
                "online" | "electronic" | "www" | "webpage" | "software" | "url"
            );
            let keep = !web || !e.field("doi").trim().is_empty();
            if !keep {
                *report.by_verdict.entry("skipped_web".into()).or_default() += 1;
            }
            keep
        })
        .collect();
    if todo.len() > max_entries {
        report.truncated = true;
        todo.truncate(max_entries);
    }
    report.checked = todo.len();
    progress(format!("Looking up {} references", todo.len()));
    let dois: Vec<String> = {
        let mut d: Vec<String> = todo
            .iter()
            .map(|e| normalize_doi(e.field("doi")))
            .filter(|d| !d.is_empty())
            .collect();
        d.sort();
        d.dedup();
        d
    };
    let by_doi = client.by_dois(&dois).await?;
    for (n, e) in todo.iter().enumerate() {
        if n % 10 == 0 && n > 0 {
            progress(format!("Checked {n} of {} references", todo.len()));
        }
        let doi = by_doi.get(&normalize_doi(e.field("doi")));
        let needs_search = !matches!(doi, Some(Lookup::NotFound))
            && !matches!(doi, Some(Lookup::Found(p)) if p["title"].as_str().is_some_and(|t| !t.is_empty()));
        let search = if needs_search && !e.field("title").trim().is_empty() {
            Some(client.search(&strip_latex(e.field("title"))).await?)
        } else {
            None
        };
        let verdict = verify::verify(e, doi, search.as_ref());
        *report.by_verdict.entry(verdict.kind().into()).or_default() += 1;
        if let Some((severity, message)) = verdict.report(e) {
            problems.push(Problem {
                key: e.key.clone(),
                verdict: verdict.kind().into(),
                severity,
                message,
                bib_file: Some(e.file.clone()),
                cited_at: cited
                    .get(&e.key)
                    .map(|c| (c.file.clone(), c.command.clone())),
            });
        }
    }
    problems.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.key.cmp(&b.key)));
    report.problems = problems;
    Ok(report)
}
