//! JSON schemas for structured model output (shared by all providers).

use serde_json::{Value, json};

/// `{ comments: [{ quote, comment, severity }] }`, the output of every LLM reviewer.
pub fn comments() -> Value {
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

/// Paper type, summary, and one category per section.
pub fn classification(categories: &[&str]) -> Value {
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
                        "category": { "type": "string", "enum": categories }
                    }
                }
            }
        }
    })
}
