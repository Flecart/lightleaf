//! The `lightleaf` pack: one classification call, then a compact set of section and
//! whole-paper reviewers that all see the full paper (cached once) and focus on their part.
//! Prompts: `reviewer/packs/lightleaf/*.md` (Lightleaf's own, AGPL-3.0-only).

use super::Files;
use crate::{
    latex::{self, Document},
    llm::Request,
    review::{BoxFuture, Ctx, Options, Pack, Plan, PromptReviewer, Reviewer},
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

pub const FILES: &[(&str, &str)] = &[
    (
        "system.md",
        include_str!("../../../packs/lightleaf/system.md"),
    ),
    (
        "classify.md",
        include_str!("../../../packs/lightleaf/classify.md"),
    ),
    (
        "abstract.md",
        include_str!("../../../packs/lightleaf/abstract.md"),
    ),
    (
        "introduction.md",
        include_str!("../../../packs/lightleaf/introduction.md"),
    ),
    (
        "related_work.md",
        include_str!("../../../packs/lightleaf/related_work.md"),
    ),
    (
        "methods.md",
        include_str!("../../../packs/lightleaf/methods.md"),
    ),
    (
        "results.md",
        include_str!("../../../packs/lightleaf/results.md"),
    ),
    (
        "conclusion.md",
        include_str!("../../../packs/lightleaf/conclusion.md"),
    ),
    (
        "appendix.md",
        include_str!("../../../packs/lightleaf/appendix.md"),
    ),
    (
        "writing_style.md",
        include_str!("../../../packs/lightleaf/writing_style.md"),
    ),
    (
        "latex_formatting.md",
        include_str!("../../../packs/lightleaf/latex_formatting.md"),
    ),
    (
        "figures_tables.md",
        include_str!("../../../packs/lightleaf/figures_tables.md"),
    ),
    (
        "venue.md",
        include_str!("../../../packs/lightleaf/venue.md"),
    ),
];

const CATEGORIES: &[&str] = &[
    "abstract",
    "introduction",
    "related_work",
    "background",
    "methods",
    "experiments",
    "results",
    "discussion",
    "conclusion",
    "appendix",
    "other",
];

enum Scope {
    Whole,
    Abstract,
    Categories(&'static [&'static str]),
}

struct Def {
    id: &'static str,
    name: &'static str,
    scope: Scope,
    max_comments: usize,
    venue_only: bool,
}

const fn def(id: &'static str, name: &'static str, scope: Scope, max_comments: usize) -> Def {
    Def {
        id,
        name,
        scope,
        max_comments,
        venue_only: false,
    }
}

const DEFS: &[Def] = &[
    def("abstract", "Abstract", Scope::Abstract, 4),
    def(
        "introduction",
        "Introduction",
        Scope::Categories(&["introduction"]),
        6,
    ),
    def(
        "related_work",
        "Related work",
        Scope::Categories(&["related_work", "background"]),
        5,
    ),
    def("methods", "Methods", Scope::Categories(&["methods"]), 8),
    def(
        "results",
        "Results",
        Scope::Categories(&["experiments", "results", "discussion"]),
        8,
    ),
    def(
        "conclusion",
        "Conclusion",
        Scope::Categories(&["conclusion"]),
        3,
    ),
    def("appendix", "Appendix", Scope::Categories(&["appendix"]), 4),
    def("writing_style", "Writing", Scope::Whole, 8),
    def("latex_formatting", "LaTeX", Scope::Whole, 6),
    def("figures_tables", "Figures and tables", Scope::Whole, 5),
    Def {
        venue_only: true,
        ..def("venue", "Venue", Scope::Whole, 5)
    },
];

pub struct Lightleaf {
    files: Files,
}

impl Lightleaf {
    pub fn new(files: Files) -> Self {
        Lightleaf { files }
    }
}

fn comments_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["comments"],
        "properties": { "comments": { "type": "array", "items": {
            "type": "object", "additionalProperties": false, "required": ["quote", "comment", "severity"],
            "properties": {
                "quote": { "type": "string", "description": "Exact passage from the LaTeX source, 20-200 characters." },
                "comment": { "type": "string", "description": "Plain-text comment to the author." },
                "severity": { "type": "string", "enum": ["suggestion", "warning", "critical"] }
            }
        }}}
    })
}

fn classify_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["paper_type", "summary", "sections"],
        "properties": {
            "paper_type": { "type": "string" },
            "summary": { "type": "string" },
            "sections": { "type": "array", "items": {
                "type": "object", "additionalProperties": false, "required": ["title", "category"],
                "properties": { "title": { "type": "string" }, "category": { "type": "string", "enum": CATEGORIES } }
            }}
        }
    })
}

/// Titles plus the first words of each section, so vague titles ("Our approach") classify well.
fn outline(merged: &str) -> String {
    let lines: String = latex::sections(merged)
        .iter()
        .map(|s| {
            let body = merged[s.start..s.end]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let excerpt: String = body.chars().skip(s.title.len() + 10).take(160).collect();
            format!(
                "- {}{}: {excerpt}…\n",
                s.title,
                if s.in_appendix { " (appendix)" } else { "" }
            )
        })
        .collect();
    if lines.is_empty() {
        "(no sections found)\n".into()
    } else {
        lines
    }
}

impl Pack for Lightleaf {
    fn id(&self) -> &'static str {
        "lightleaf"
    }

    fn plan<'a>(
        &'a self,
        ctx: &'a Ctx,
        doc: &'a Document,
        o: &'a Options,
    ) -> BoxFuture<'a, anyhow::Result<Plan>> {
        Box::pin(async move {
            let merged = doc.merged();
            let document = Arc::new(format!("<paper>\n{merged}</paper>"));
            let system = self.files.must("system.md").trim().to_string();
            let task = format!(
                "{}\n{}",
                self.files.must("classify.md").trim(),
                outline(&merged)
            );
            let schema = classify_schema();
            let v = ctx
                .llm
                .json(
                    &Request {
                        system: &system,
                        document: &document,
                        task: &task,
                        schema: &schema,
                        max_tokens: 8000,
                    },
                    &ctx.usage,
                )
                .await?;
            let paper_type = v["paper_type"].as_str().unwrap_or("unknown").to_string();
            let summary = v["summary"].as_str().unwrap_or("").to_string();
            let mut by_cat: HashMap<String, Vec<String>> = HashMap::new();
            for s in v["sections"].as_array().into_iter().flatten() {
                if let (Some(t), Some(c)) = (s["title"].as_str(), s["category"].as_str()) {
                    by_cat.entry(c.into()).or_default().push(t.into());
                }
            }
            let has_abstract = latex::sections(&merged)
                .iter()
                .any(|s| s.title == "Abstract");
            let schema = Arc::new(comments_schema());
            let mut reviewers: Vec<Arc<dyn Reviewer>> = Vec::new();
            for d in DEFS {
                if d.venue_only && o.venue.trim().is_empty() {
                    continue;
                }
                let scope = match &d.scope {
                    Scope::Whole => "Review the whole paper from this angle.".to_string(),
                    Scope::Abstract if has_abstract => "Review only the abstract.".to_string(),
                    Scope::Abstract => continue,
                    Scope::Categories(cats) => {
                        let titles: Vec<String> = cats
                            .iter()
                            .flat_map(|c| by_cat.get(*c).cloned().unwrap_or_default())
                            .collect();
                        if titles.is_empty() {
                            continue;
                        }
                        format!(
                            "Review only these sections: {}.",
                            titles
                                .iter()
                                .map(|t| format!("\"{t}\""))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                };
                let mut task = format!(
                    "Paper type: {paper_type}.\nSummary: {summary}\n\nYour role: {}\n\n{scope} Leave at most {} comments.",
                    self.files.must(&format!("{}.md", d.id)).trim(),
                    d.max_comments
                );
                if !o.venue.trim().is_empty() {
                    task.push_str(&format!("\nTarget venue: {}.", o.venue.trim()));
                }
                if !o.focus.trim().is_empty() {
                    task.push_str(&format!(
                        "\nThe author asks you to keep this in mind: {}",
                        o.focus.trim()
                    ));
                }
                if o.strict {
                    task.push_str("\nOnly report issues that would materially affect a reviewer's assessment; no suggestions.");
                }
                reviewers.push(Arc::new(PromptReviewer {
                    id: d.id.into(),
                    name: d.name.into(),
                    system: system.clone(),
                    document: document.clone(),
                    task,
                    schema: schema.clone(),
                    quote_field: "quote",
                    max_comments: d.max_comments,
                    broad: matches!(d.scope, Scope::Whole),
                }));
            }
            Ok(Plan {
                paper_type,
                summary,
                reviewers,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reviewer_has_a_prompt() {
        let files = Files::new(FILES, None);
        for d in DEFS {
            assert!(
                files
                    .get(&format!("{}.md", d.id))
                    .is_some_and(|t| !t.trim().is_empty()),
                "missing prompt for {}",
                d.id
            );
            if let Scope::Categories(cats) = d.scope {
                assert!(
                    cats.iter().all(|c| CATEGORIES.contains(c)),
                    "{} uses an unknown category",
                    d.id
                );
            }
        }
    }
}
