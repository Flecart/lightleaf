//! A reviewer backed by a model call: a prompt (role), a scope (some section categories, or
//! the whole paper) and a comment budget. All prompt-driven reviewers are this one type,
//! configured from the table in [`super::registry`].

use super::{BoxFuture, Finding, ReviewInput, Reviewer, Severity, schema};
use crate::llm::Request;
use std::sync::Arc;

/// Which part of the paper a reviewer focuses on.
pub enum Scope {
    Whole,
    Abstract,
    /// Sections classified into any of these categories.
    Categories(&'static [&'static str]),
}

/// When a reviewer runs, beyond having something in scope.
pub enum Condition {
    Always,
    /// Only when the author named a target venue.
    VenueGiven,
}

pub struct LlmReviewer {
    pub id: &'static str,
    pub name: &'static str,
    /// Name of the prompt file (`prompts/<prompt>.md`).
    pub prompt: &'static str,
    pub scope: Scope,
    pub condition: Condition,
    pub max_comments: usize,
}

impl LlmReviewer {
    fn scope_text(&self, input: &ReviewInput) -> String {
        match &self.scope {
            Scope::Whole => "Review the whole paper from this angle.".into(),
            Scope::Abstract => "Review only the abstract.".into(),
            Scope::Categories(c) => {
                let titles = input.classification.titles(c);
                format!(
                    "Review only these sections: {}.",
                    titles
                        .iter()
                        .map(|t| format!("\"{t}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }

    fn task(&self, input: &ReviewInput) -> String {
        let c = &input.classification;
        let mut task = format!(
            "Paper type: {}.\nSummary: {}\n\nYour role: {}\n\n{} Leave at most {} comments.",
            c.paper_type,
            c.summary,
            input.prompts.get(self.prompt),
            self.scope_text(input),
            self.max_comments
        );
        let o = &input.options;
        if !o.venue.trim().is_empty() {
            task.push_str(&format!("\nTarget venue: {}.", o.venue.trim()));
        }
        if !o.focus.trim().is_empty() {
            task.push_str(&format!(
                "\nThe author asks you to keep this in mind: {}",
                o.focus.trim()
            ));
        }
        task
    }
}

impl Reviewer for LlmReviewer {
    fn id(&self) -> &str {
        self.id
    }

    fn name(&self) -> &str {
        self.name
    }

    fn max_findings(&self) -> usize {
        self.max_comments
    }

    fn applies(&self, input: &ReviewInput) -> bool {
        let condition = match self.condition {
            Condition::Always => true,
            Condition::VenueGiven => !input.options.venue.trim().is_empty(),
        };
        let in_scope = match &self.scope {
            Scope::Whole => true,
            Scope::Abstract => input.classification.has_abstract,
            Scope::Categories(c) => !input.classification.titles(c).is_empty(),
        };
        condition && in_scope
    }

    fn review(
        self: Arc<Self>,
        input: Arc<ReviewInput>,
    ) -> BoxFuture<'static, anyhow::Result<Vec<Finding>>> {
        Box::pin(async move {
            let task = self.task(&input);
            let schema = schema::comments();
            let req = Request {
                system: &input.system,
                document: &input.document,
                task: &task,
                schema: &schema,
                max_tokens: 16000,
            };
            let v = input.llm.json(&req, &input.usage).await?;
            Ok(v["comments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    Some(Finding {
                        quote: c["quote"].as_str()?.to_string(),
                        text: c["comment"].as_str()?.to_string(),
                        severity: Severity::parse(c["severity"].as_str().unwrap_or("")),
                    })
                })
                .collect())
        })
    }
}
