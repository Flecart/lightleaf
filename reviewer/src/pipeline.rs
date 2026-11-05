//! The review workflow: classify the paper's sections, run specialised reviewer agents in
//! parallel, then deduplicate, rank and anchor their comments in the LaTeX source.
//!
//! 1. Collect the main file and its `\input`s; split into sections (no LLM).
//! 2. One call classifies the paper type and each section's category. It also writes the
//!    prompt cache that every following call reads, since they share the same prefix.
//! 3. Agents run in parallel (bounded concurrency, per-agent timeout). A failed agent is
//!    reported and skipped; it never fails the job.
//! 4. Comments whose quote cannot be found in the source are dropped; overlapping, similar
//!    comments are merged; the most severe are kept up to the configured maximum.

use crate::{
    config::Config,
    latex::{self, Document},
    llm::{Llm, Request, Usage},
    prompts::Prompts,
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinSet};

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

/// A reviewer agent: what it reviews and with which instructions.
struct Agent {
    id: &'static str,
    name: &'static str,
    /// Section categories it reviews; empty = the whole paper.
    categories: &'static [&'static str],
    max_comments: u32,
}

const AGENTS: &[Agent] = &[
    Agent {
        id: "abstract",
        name: "Abstract",
        categories: &["abstract"],
        max_comments: 4,
    },
    Agent {
        id: "introduction",
        name: "Introduction",
        categories: &["introduction"],
        max_comments: 6,
    },
    Agent {
        id: "related_work",
        name: "Related work",
        categories: &["related_work", "background"],
        max_comments: 5,
    },
    Agent {
        id: "methods",
        name: "Methods",
        categories: &["methods"],
        max_comments: 8,
    },
    Agent {
        id: "results",
        name: "Results",
        categories: &["experiments", "results", "discussion"],
        max_comments: 8,
    },
    Agent {
        id: "conclusion",
        name: "Conclusion",
        categories: &["conclusion"],
        max_comments: 3,
    },
    Agent {
        id: "appendix",
        name: "Appendix",
        categories: &["appendix"],
        max_comments: 4,
    },
    Agent {
        id: "writing_style",
        name: "Writing",
        categories: &[],
        max_comments: 8,
    },
    Agent {
        id: "latex_formatting",
        name: "LaTeX",
        categories: &[],
        max_comments: 6,
    },
    Agent {
        id: "figures_tables",
        name: "Figures and tables",
        categories: &[],
        max_comments: 5,
    },
    Agent {
        id: "venue",
        name: "Venue",
        categories: &[],
        max_comments: 5,
    },
];

#[cfg(test)]
pub fn agent_ids() -> Vec<&'static str> {
    AGENTS.iter().map(|a| a.id).collect()
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Suggestion,
    Warning,
    Critical,
}

impl Severity {
    fn parse(s: &str) -> Severity {
        match s {
            "critical" => Severity::Critical,
            "warning" => Severity::Warning,
            _ => Severity::Suggestion,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Critical => "critical",
            Severity::Warning => "warning",
            Severity::Suggestion => "suggestion",
        }
    }
}

/// A comment anchored in a source file.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Comment {
    pub agent: String,
    pub severity: Severity,
    pub text: String,
    pub quote: String,
    pub file: String,
    /// Byte range of the quote in the file at review time.
    #[serde(skip)]
    pub range: (usize, usize),
}

pub struct Options {
    pub venue: String,
    pub focus: String,
}

pub struct Outcome {
    pub paper_type: String,
    pub summary: String,
    pub comments: Vec<Comment>,
    pub failed_agents: Vec<(String, String)>,
    pub unanchored: usize,
    pub merged_duplicates: usize,
    pub dropped_over_limit: usize,
}

fn comments_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["comments"],
        "properties": {
            "comments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["quote", "comment", "severity"],
                    "properties": {
                        "quote": { "type": "string", "description": "Exact passage from the LaTeX source, 20-200 characters." },
                        "comment": { "type": "string", "description": "Plain-text comment to the author." },
                        "severity": { "type": "string", "enum": ["suggestion", "warning", "critical"] }
                    }
                }
            }
        }
    })
}

fn classify_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["paper_type", "summary", "sections"],
        "properties": {
            "paper_type": { "type": "string" },
            "summary": { "type": "string" },
            "sections": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["title", "category"],
                    "properties": {
                        "title": { "type": "string" },
                        "category": { "type": "string", "enum": CATEGORIES }
                    }
                }
            }
        }
    })
}

/// Bigram Dice similarity of two strings (0..=1), used to spot duplicate comments.
fn dice(a: &str, b: &str) -> f64 {
    let grams = |s: &str| {
        let chars: Vec<char> = s
            .to_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let mut m: HashMap<(char, char), usize> = HashMap::new();
        for w in chars.windows(2) {
            *m.entry((w[0], w[1])).or_default() += 1;
        }
        m
    };
    let (ga, gb) = (grams(a), grams(b));
    let (na, nb): (usize, usize) = (ga.values().sum(), gb.values().sum());
    if na + nb == 0 {
        return 0.0;
    }
    let common: usize = ga
        .iter()
        .map(|(k, v)| (*v).min(*gb.get(k).unwrap_or(&0)))
        .sum();
    2.0 * common as f64 / (na + nb) as f64
}

/// Merges comments on overlapping passages that say similar things (keeps the most severe).
fn dedup(mut comments: Vec<Comment>) -> (Vec<Comment>, usize) {
    comments.sort_by(|a, b| b.severity.cmp(&a.severity));
    let mut kept: Vec<Comment> = Vec::new();
    let mut merged = 0;
    'next: for c in comments {
        for k in &kept {
            if k.file != c.file {
                continue;
            }
            let overlap = c
                .range
                .1
                .min(k.range.1)
                .saturating_sub(c.range.0.max(k.range.0));
            let shorter = (c.range.1 - c.range.0).min(k.range.1 - k.range.0).max(1);
            if (overlap * 2 >= shorter && dice(&c.text, &k.text) >= 0.4)
                || dice(&c.text, &k.text) >= 0.8
            {
                merged += 1;
                continue 'next;
            }
        }
        kept.push(c);
    }
    (kept, merged)
}

pub struct Ctx {
    pub llm: Arc<Llm>,
    pub cfg: Arc<Config>,
    pub prompts: Arc<Prompts>,
    pub usage: Arc<Usage>,
}

/// Runs the whole review and returns anchored comments (not yet written to the files).
pub async fn review(
    ctx: &Ctx,
    doc: &Document,
    opts: &Options,
    progress: impl Fn(String),
) -> anyhow::Result<Outcome> {
    let merged = doc.merged();
    if merged.len() > ctx.cfg.max_input_chars {
        anyhow::bail!(
            "the document is too long to review ({} characters, limit {}); raise LIGHTLEAF_REVIEW_MAX_INPUT_CHARS",
            merged.len(),
            ctx.cfg.max_input_chars
        );
    }
    let document = format!("<paper>\n{merged}</paper>");
    let system = ctx.prompts.get("system");
    let sections = latex::sections(&merged);

    // 1. Classification.
    progress("Reading the paper".into());
    // Titles plus the first words of each section, so vague titles ("Our approach") classify well.
    let outline: String = sections
        .iter()
        .map(|s| {
            let body: String = merged[s.start..s.end]
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
    let task = format!(
        "{}\n{}",
        ctx.prompts.get("classify"),
        if outline.is_empty() {
            "(no sections found)\n".into()
        } else {
            outline
        }
    );
    let schema = classify_schema();
    let class = ctx
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
    let paper_type = class["paper_type"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let summary = class["summary"].as_str().unwrap_or("").to_string();
    let mut by_category: HashMap<String, Vec<String>> = HashMap::new();
    for s in class["sections"].as_array().into_iter().flatten() {
        if let (Some(t), Some(c)) = (s["title"].as_str(), s["category"].as_str()) {
            by_category
                .entry(c.to_string())
                .or_default()
                .push(t.to_string());
        }
    }
    if sections.iter().any(|s| s.title == "Abstract") {
        by_category.entry("abstract".into()).or_default();
    }

    // 2. Agents in parallel.
    let mut plan = Vec::new();
    for a in AGENTS {
        if ctx.cfg.disabled_agents.iter().any(|d| d == a.id)
            || (a.id == "venue" && opts.venue.trim().is_empty())
        {
            continue;
        }
        let titles: Vec<String> = a
            .categories
            .iter()
            .flat_map(|c| by_category.get(*c).cloned().unwrap_or_default())
            .collect();
        if !a.categories.is_empty()
            && titles.is_empty()
            && !(a.id == "abstract" && by_category.contains_key("abstract"))
        {
            continue;
        }
        plan.push((a, titles));
    }
    progress(format!("Running {} reviewers", plan.len()));
    let sem = Arc::new(Semaphore::new(ctx.cfg.agent_concurrency));
    let schema = Arc::new(comments_schema());
    let (system, document) = (Arc::new(system), Arc::new(document));
    let mut set = JoinSet::new();
    for (agent, titles) in plan {
        let scope = if agent.categories.is_empty() {
            "Review the whole paper from this angle.".to_string()
        } else if agent.id == "abstract" {
            "Review only the abstract.".to_string()
        } else {
            format!(
                "Review only these sections: {}.",
                titles
                    .iter()
                    .map(|t| format!("\"{t}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let mut task = format!(
            "Paper type: {paper_type}.\nSummary: {summary}\n\nYour role: {}\n\n{scope} Leave at most {} comments.",
            ctx.prompts.get(agent.id),
            agent.max_comments
        );
        if !opts.venue.trim().is_empty() {
            task.push_str(&format!("\nTarget venue: {}.", opts.venue.trim()));
        }
        if !opts.focus.trim().is_empty() {
            task.push_str(&format!(
                "\nThe author asks you to keep this in mind: {}",
                opts.focus.trim()
            ));
        }
        let (llm, usage, sem, schema, system, document) = (
            ctx.llm.clone(),
            ctx.usage.clone(),
            sem.clone(),
            schema.clone(),
            system.clone(),
            document.clone(),
        );
        let timeout = Duration::from_secs(ctx.cfg.agent_timeout_secs);
        set.spawn(async move {
            let _permit = sem.acquire_owned().await;
            let req = Request {
                system: &system,
                document: &document,
                task: &task,
                schema: &schema,
                max_tokens: 16000,
            };
            let res = tokio::time::timeout(timeout, llm.json(&req, &usage)).await;
            let res = match res {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!("timed out after {} s", timeout.as_secs())),
            };
            (agent, res)
        });
    }

    let mut raw = Vec::new();
    let mut failed = Vec::new();
    let mut unanchored = 0;
    while let Some(joined) = set.join_next().await {
        let Ok((agent, res)) = joined else { continue };
        match res {
            Ok(v) => {
                let list = v["comments"].as_array().cloned().unwrap_or_default();
                for c in list.into_iter().take(agent.max_comments as usize) {
                    let (Some(quote), Some(text)) = (c["quote"].as_str(), c["comment"].as_str())
                    else {
                        continue;
                    };
                    let text: String = text.trim().chars().take(1200).collect();
                    if text.is_empty() {
                        continue;
                    }
                    let found = doc.files.iter().find_map(|f| {
                        latex::find_quote(&f.text, quote).map(|r| (f.path.clone(), r))
                    });
                    match found {
                        Some((file, range)) => raw.push(Comment {
                            agent: agent.name.to_string(),
                            severity: Severity::parse(c["severity"].as_str().unwrap_or("")),
                            text,
                            quote: quote.to_string(),
                            file,
                            range,
                        }),
                        None => unanchored += 1,
                    }
                }
                progress(format!("{} reviewer finished", agent.name));
            }
            Err(e) => {
                tracing::warn!("agent {} failed: {e:#}", agent.id);
                failed.push((agent.name.to_string(), format!("{e:#}")));
            }
        }
    }

    // 3. Deduplicate and keep the most important.
    let (mut comments, merged_duplicates) = dedup(raw);
    comments.sort_by(|a, b| b.severity.cmp(&a.severity));
    let dropped_over_limit = comments.len().saturating_sub(ctx.cfg.max_comments);
    comments.truncate(ctx.cfg.max_comments);
    comments.sort_by(|a, b| a.file.cmp(&b.file).then(a.range.0.cmp(&b.range.0)));
    Ok(Outcome {
        paper_type,
        summary,
        comments,
        failed_agents: failed,
        unanchored,
        merged_duplicates,
        dropped_over_limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(sev: Severity, text: &str, range: (usize, usize)) -> Comment {
        Comment {
            agent: "A".into(),
            severity: sev,
            text: text.into(),
            quote: String::new(),
            file: "main.tex".into(),
            range,
        }
    }

    #[test]
    fn dedup_merges_similar_overlapping_comments() {
        let (kept, merged) = dedup(vec![
            c(
                Severity::Suggestion,
                "This claim needs a citation to prior work.",
                (10, 60),
            ),
            c(
                Severity::Warning,
                "This claim needs a citation of prior work!",
                (20, 70),
            ),
            c(
                Severity::Suggestion,
                "Define the acronym before using it.",
                (15, 40),
            ),
        ]);
        assert_eq!(merged, 1);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().any(|k| k.severity == Severity::Warning));
    }

    #[test]
    fn dice_similarity() {
        assert!(dice("needs a citation", "needs a citation") > 0.99);
        assert!(dice("needs a citation", "define the acronym") < 0.3);
    }

    #[test]
    fn agent_table_is_consistent() {
        for a in AGENTS {
            for cat in a.categories {
                assert!(
                    CATEGORIES.contains(cat),
                    "{} uses unknown category {cat}",
                    a.id
                );
            }
        }
    }
}
