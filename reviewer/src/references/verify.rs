//! Verdicts for one bibliography entry, with upstream's thresholds and messages.

use super::{
    bib::Entry,
    normalize::{
        normalize_author, normalize_doi, strip_latex, surnames, title_similarity, venues_agree,
    },
    s2::Lookup,
};
use crate::review::Severity;
use serde_json::Value;

pub const TITLE_MATCH_STRONG: f64 = 0.85;
pub const TITLE_MATCH_CANDIDATE: f64 = 0.70;
const DOI_TITLE_FLOOR: f64 = 0.5;

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Verified,
    /// Fields disagree with the matched record.
    Mismatch {
        record: String,
        issues: Vec<(String, Severity, String)>,
    },
    /// No close match anywhere.
    Fabricated {
        best_sim: f64,
        closest: Option<String>,
    },
    /// A plausible but not certain match, or the service was unavailable.
    Unverified,
    DoiNotFound,
    DoiMismatch {
        record: String,
    },
    NoTitle,
}

impl Verdict {
    pub fn kind(&self) -> &'static str {
        match self {
            Verdict::Verified => "verified",
            Verdict::Mismatch { .. } => "mismatch",
            Verdict::Fabricated { .. } => "fabricated",
            Verdict::Unverified => "unverified",
            Verdict::DoiNotFound => "doi_not_found",
            Verdict::DoiMismatch { .. } => "doi_mismatch",
            Verdict::NoTitle => "no_title",
        }
    }

    /// Severity and message to show, or `None` for nothing to report.
    pub fn report(&self, e: &Entry) -> Option<(Severity, String)> {
        Some(match self {
            Verdict::Verified | Verdict::NoTitle => return None,
            Verdict::Fabricated { best_sim, closest } => (
                Severity::Critical,
                format!(
                    "Reference may be fabricated. No close match in Semantic Scholar (best title similarity {best_sim:.2}). {}",
                    match closest {
                        Some(c) => format!("Closest hit: {c}."),
                        None => "No candidates returned by S2 search.".into(),
                    }
                ),
            ),
            Verdict::DoiNotFound => (
                Severity::Critical,
                format!(
                    "DOI \"{}\" was not found in Semantic Scholar. Verify the reference details.",
                    e.field("doi")
                ),
            ),
            Verdict::DoiMismatch { record } => (
                Severity::Critical,
                format!(
                    "DOI \"{}\" resolves to a different paper: {record}. Check the citation.",
                    e.field("doi")
                ),
            ),
            Verdict::Unverified => (
                Severity::Warning,
                format!(
                    "Could not verify in Semantic Scholar (no result above {TITLE_MATCH_CANDIDATE} title similarity). This is common for workshop papers, tech reports, or pre-2000 work \u{2014} confirm details manually."
                ),
            ),
            Verdict::Mismatch { record, issues } => {
                let sev = if issues.iter().any(|(_, s, _)| *s == Severity::Critical) {
                    Severity::Critical
                } else {
                    Severity::Warning
                };
                let lines: Vec<String> = issues
                    .iter()
                    .map(|(f, _, d)| format!("- {f}: {d}"))
                    .collect();
                (
                    sev,
                    format!(
                        "Reference fields disagree with Semantic Scholar record {record}:\n{}",
                        lines.join("\n")
                    ),
                )
            }
        })
    }
}

/// `"Title" (2020, Venue)` for messages.
pub fn describe(p: &Value) -> String {
    let year = p["year"]
        .as_u64()
        .map(|y| y.to_string())
        .unwrap_or_else(|| "unknown year".into());
    let venue = p["venue"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or(p["publicationVenue"]["name"].as_str())
        .unwrap_or("no venue");
    format!(
        "\"{}\" ({year}, {venue})",
        p["title"].as_str().unwrap_or("")
    )
}

/// Field-by-field comparison with a matched record (upstream `compareEntryToPaper`).
pub fn compare(e: &Entry, p: &Value) -> Vec<(String, Severity, String)> {
    let mut issues = Vec::new();
    let bib = surnames(e.field("author"));
    let s2: Vec<String> = p["authors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["name"].as_str())
        .map(|n| normalize_author(n.split_whitespace().last().unwrap_or("")))
        .filter(|n| !n.is_empty())
        .collect();
    if let (Some(fb), Some(fs)) = (bib.first(), s2.first())
        && fb != fs
    {
        let overlap = bib.iter().filter(|n| s2.contains(n)).count();
        if (overlap as f64) / (bib.len().max(1) as f64) < 0.5 {
            issues.push((
                "author".into(),
                Severity::Critical,
                format!("bib first author \"{fb}\" but Semantic Scholar lists \"{fs}\""),
            ));
        } else {
            issues.push(("author".into(), Severity::Warning, format!("first-author surname differs: bib \"{fb}\" vs S2 \"{fs}\" (other authors do match)")));
        }
    }
    let year = |s: &str| {
        s.trim()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<i64>()
            .ok()
    };
    if let (Some(by), Some(sy)) = (year(e.field("year")), p["year"].as_i64()) {
        let d = (by - sy).abs();
        if d > 3 {
            issues.push((
                "year".into(),
                Severity::Critical,
                format!("bib year {by} vs S2 year {sy}"),
            ));
        } else if d > 1 {
            issues.push((
                "year".into(),
                Severity::Warning,
                format!("bib year {by} vs S2 year {sy}"),
            ));
        }
    }
    let bib_venue = [
        e.field("booktitle"),
        e.field("journal"),
        e.field("publisher"),
    ]
    .into_iter()
    .find(|v| !v.is_empty())
    .unwrap_or("");
    let s2_venue = p["publicationVenue"]["name"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or(p["venue"].as_str())
        .unwrap_or("");
    if venues_agree(bib_venue, s2_venue) == Some(false) {
        issues.push((
            "venue".into(),
            Severity::Warning,
            format!(
                "bib venue \"{}\" vs S2 venue \"{s2_venue}\"",
                strip_latex(bib_venue)
            ),
        ));
    }
    let (bd, sd) = (
        normalize_doi(e.field("doi")),
        normalize_doi(p["externalIds"]["DOI"].as_str().unwrap_or("")),
    );
    if !bd.is_empty() && !sd.is_empty() && bd != sd {
        issues.push((
            "doi".into(),
            Severity::Critical,
            format!("bib DOI \"{bd}\" vs S2 DOI \"{sd}\""),
        ));
    }
    issues
}

fn from_record(e: &Entry, p: &Value) -> Verdict {
    let issues = compare(e, p);
    if issues.is_empty() {
        Verdict::Verified
    } else {
        Verdict::Mismatch {
            record: describe(p),
            issues,
        }
    }
}

/// Upstream `verifyEntry`: DOI route first, then title search.
pub fn verify(
    e: &Entry,
    doi: Option<&Lookup<Value>>,
    search: Option<&Lookup<Vec<Value>>>,
) -> Verdict {
    let title = e.field("title").trim();
    match doi {
        Some(Lookup::NotFound) => return Verdict::DoiNotFound,
        Some(Lookup::Found(p)) if p["title"].as_str().is_some_and(|t| !t.is_empty()) => {
            if !title.is_empty()
                && title_similarity(title, p["title"].as_str().unwrap_or("")) < DOI_TITLE_FLOOR
            {
                return Verdict::DoiMismatch {
                    record: describe(p),
                };
            }
            return from_record(e, p);
        }
        _ => {} // no DOI, unavailable, or a record without a title: fall through
    }
    if title.is_empty() {
        return Verdict::NoTitle;
    }
    let candidates = match search {
        Some(Lookup::Found(c)) => c,
        Some(Lookup::NotFound) => {
            return Verdict::Fabricated {
                best_sim: 0.0,
                closest: None,
            };
        }
        Some(Lookup::Unavailable) | None => return Verdict::Unverified,
    };
    let mut best: Option<(&Value, f64)> = None;
    for c in candidates {
        let s = title_similarity(title, c["title"].as_str().unwrap_or(""));
        if best.is_none_or(|(_, b)| s > b) {
            best = Some((c, s));
        }
    }
    let Some((p, sim)) = best else {
        return Verdict::Fabricated {
            best_sim: 0.0,
            closest: None,
        };
    };
    if sim < TITLE_MATCH_CANDIDATE {
        return Verdict::Fabricated {
            best_sim: sim,
            closest: Some(describe(p)),
        };
    }
    match from_record(e, p) {
        Verdict::Verified if sim < TITLE_MATCH_STRONG => Verdict::Unverified,
        v => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(fields: &[(&str, &str)]) -> Entry {
        Entry {
            key: "k".into(),
            kind: "article".into(),
            file: "r.bib".into(),
            fields: fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn paper(title: &str, first: &str, year: i64) -> Value {
        json!({"title": title, "year": year, "venue": "NeurIPS", "authors": [{"name": format!("Ann {first}")}, {"name": "Bob Lee"}], "externalIds": {"DOI": "10.1/abc"}})
    }

    #[test]
    fn verdicts_follow_upstream_thresholds() {
        let e = entry(&[
            ("title", "Attention Is All You Need"),
            ("author", "Vaswani, Ann and Lee, Bob"),
            ("year", "2017"),
            (
                "booktitle",
                "Advances in Neural Information Processing Systems",
            ),
        ]);
        let found = |p: Value| Some(Lookup::Found(vec![p]));
        assert_eq!(
            verify(
                &e,
                None,
                found(paper("Attention is all you need", "Vaswani", 2017)).as_ref()
            ),
            Verdict::Verified
        );
        // Plausible (0.70-0.85) and consistent -> unverified.
        assert_eq!(
            verify(
                &e,
                None,
                found(paper("Attention is all you need, really!", "Vaswani", 2017)).as_ref()
            ),
            Verdict::Unverified
        );
        assert!(matches!(
            verify(
                &e,
                None,
                found(paper("Something else entirely", "X", 2017)).as_ref()
            ),
            Verdict::Fabricated { .. }
        ));
        let wrong_year = verify(
            &e,
            None,
            found(paper("Attention is all you need", "Vaswani", 2012)).as_ref(),
        );
        assert!(
            matches!(&wrong_year, Verdict::Mismatch { issues, .. } if issues[0].1 == Severity::Critical)
        );
        assert_eq!(
            verify(&e, None, Some(&Lookup::Unavailable)),
            Verdict::Unverified,
            "outages never mean fabricated"
        );
        assert_eq!(verify(&entry(&[]), None, None), Verdict::NoTitle);
    }

    #[test]
    fn doi_route() {
        let e = entry(&[
            ("title", "Attention Is All You Need"),
            ("doi", "https://doi.org/10.1/ABC"),
            ("author", "Vaswani, Ann"),
            ("year", "2017"),
        ]);
        assert_eq!(
            verify(&e, Some(&Lookup::NotFound), None),
            Verdict::DoiNotFound
        );
        assert!(matches!(
            verify(
                &e,
                Some(&Lookup::Found(paper("Graph networks", "Vaswani", 2017))),
                None
            ),
            Verdict::DoiMismatch { .. }
        ));
        // The prefixed DOI equals the record's DOI after normalisation.
        assert_eq!(
            verify(
                &e,
                Some(&Lookup::Found(paper(
                    "Attention is all you need",
                    "Vaswani",
                    2017
                ))),
                None
            ),
            Verdict::Verified
        );
    }
}
