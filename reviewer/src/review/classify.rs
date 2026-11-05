//! Paper classification: the paper type, a short summary, and a category for every section.
//! This is the first model call of a review; it also writes the prompt cache that the
//! reviewers then read (same system prompt and document prefix).

use super::{Ctx, schema};
use crate::{latex, llm::Request};
use std::collections::HashMap;

pub const CATEGORIES: &[&str] = &[
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

#[derive(Debug, Default, Clone)]
pub struct Classification {
    pub paper_type: String,
    pub summary: String,
    /// Section titles per category, in document order.
    pub sections: HashMap<String, Vec<String>>,
    pub has_abstract: bool,
}

impl Classification {
    /// Titles of the sections in any of `categories`.
    pub fn titles(&self, categories: &[&str]) -> Vec<String> {
        categories
            .iter()
            .flat_map(|c| self.sections.get(*c).cloned().unwrap_or_default())
            .collect()
    }
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

pub async fn run(
    ctx: &Ctx,
    system: &str,
    document: &str,
    merged: &str,
) -> anyhow::Result<Classification> {
    let task = format!("{}\n{}", ctx.prompts.get("classify"), outline(merged));
    let schema = schema::classification(CATEGORIES);
    let v = ctx
        .llm
        .json(
            &Request {
                system,
                document,
                task: &task,
                schema: &schema,
                max_tokens: 8000,
            },
            &ctx.usage,
        )
        .await?;
    let mut c = Classification {
        paper_type: v["paper_type"].as_str().unwrap_or("unknown").to_string(),
        summary: v["summary"].as_str().unwrap_or("").to_string(),
        has_abstract: latex::sections(merged)
            .iter()
            .any(|s| s.title == "Abstract"),
        ..Default::default()
    };
    for s in v["sections"].as_array().into_iter().flatten() {
        if let (Some(t), Some(cat)) = (s["title"].as_str(), s["category"].as_str()) {
            c.sections
                .entry(cat.to_string())
                .or_default()
                .push(t.to_string());
        }
    }
    Ok(c)
}
