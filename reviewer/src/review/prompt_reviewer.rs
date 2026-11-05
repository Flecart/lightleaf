//! A reviewer that is one structured model call: a prepared system prompt and user message,
//! and an output schema with a list of `{<quote field>, comment, severity}` objects. Packs build
//! these with the prompts and inputs their methodology prescribes.

use super::{BoxFuture, Finding, Reviewer, Services, Severity};
use crate::llm::Request;
use serde_json::Value;
use std::sync::Arc;

pub struct PromptReviewer {
    pub id: String,
    pub name: String,
    pub system: String,
    /// Large shared prefix of the user message (cached across reviewers); may be empty.
    pub document: Arc<String>,
    pub task: String,
    pub schema: Arc<Value>,
    /// Name of the quote field in the schema (`quote`, `highlightText`, ...).
    pub quote_field: &'static str,
    pub max_comments: usize,
    pub broad: bool,
}

impl Reviewer for PromptReviewer {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn max_findings(&self) -> usize {
        self.max_comments
    }

    fn broad(&self) -> bool {
        self.broad
    }

    fn review(
        self: Arc<Self>,
        services: Arc<Services>,
    ) -> BoxFuture<'static, anyhow::Result<Vec<Finding>>> {
        Box::pin(async move {
            let req = Request {
                system: &self.system,
                document: &self.document,
                task: &self.task,
                schema: &self.schema,
                max_tokens: 16000,
            };
            let v = services.llm.json(&req, &services.usage).await?;
            Ok(parse_findings(&v, self.quote_field))
        })
    }
}

/// Reads `{comments: [{<quote_field>, comment, severity}]}`, skipping malformed entries.
pub fn parse_findings(v: &Value, quote_field: &str) -> Vec<Finding> {
    v["comments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(Finding {
                quote: c[quote_field].as_str()?.to_string(),
                text: c["comment"].as_str()?.to_string(),
                severity: Severity::parse(c["severity"].as_str().unwrap_or("")),
            })
        })
        .collect()
}
