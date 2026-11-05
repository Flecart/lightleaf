//! Strict mode, phase 5: with more than 30 comments left after deduplication, ask the model
//! which to drop; fall back to a severity ranking if its answer is unusable.

use super::PaperMentor;
use crate::{
    llm::Request,
    review::{Comment, Ctx, template::render},
};
use serde_json::json;

pub const STRICT_MAX_COMMENTS: usize = 30;

/// Keeps the `keep` highest-ranked comments (severity first, section reviewers before
/// whole-paper ones), in their original order.
pub fn severity_fallback(comments: Vec<Comment>, keep: usize) -> Vec<Comment> {
    let mut ranked: Vec<(usize, &Comment)> = comments.iter().enumerate().collect();
    let score = |c: &Comment| (c.severity.clone() as usize + 1) * 10 + usize::from(!c.broad);
    ranked.sort_by_key(|a| std::cmp::Reverse(score(a.1)));
    let mut keep_idx: Vec<usize> = ranked.into_iter().take(keep).map(|(i, _)| i).collect();
    keep_idx.sort();
    comments
        .into_iter()
        .enumerate()
        .filter(|(i, _)| keep_idx.binary_search(i).is_ok())
        .map(|(_, c)| c)
        .collect()
}

pub async fn prune(pack: &PaperMentor, ctx: &Ctx, comments: Vec<Comment>) -> Vec<Comment> {
    if comments.len() <= STRICT_MAX_COMMENTS {
        return comments;
    }
    let n = comments.len();
    let to_remove = n - STRICT_MAX_COMMENTS;
    let (count, remove) = (n.to_string(), to_remove.to_string());
    let f = |name: &str| pack.files.must(&format!("prompts/{name}")).to_string();
    let content_focus = if pack.settings.content_focus {
        f("pruning.content_focus_block.md")
    } else {
        String::new()
    };
    let system = render(
        &f("pruning.system.md"),
        &[
            ("commentCount", &count),
            ("toRemove", &remove),
            ("contentFocusBlock", &content_focus),
        ],
    );
    let list: Vec<String> = comments
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let first100: String = c.quote.chars().take(100).collect();
            render(
                &f("pruning.comment_line.md"),
                &[
                    ("index", &i.to_string()),
                    ("severity", c.severity.as_str()),
                    ("agentName", &c.agent),
                    ("highlightTextFirst100", &first100),
                    ("comment", &c.text),
                ],
            )
        })
        .collect();
    let task = render(
        &f("pruning.user.md"),
        &[
            ("commentCount", &count),
            ("toRemove", &remove),
            ("commentList", &list.join("\n\n")),
        ],
    );
    let schema = json!({
        "type": "object", "additionalProperties": false, "required": ["indicesToRemove"],
        "properties": { "indicesToRemove": {
            "type": "array", "items": { "type": "integer" },
            "description": format!("Exactly {to_remove} indices of comments to remove (0-based)")
        }}
    });
    let answer = ctx
        .llm
        .json(
            &Request {
                system: &system,
                document: "",
                task: &task,
                schema: &schema,
                max_tokens: 4000,
            },
            &ctx.usage,
        )
        .await;
    let indices: Option<std::collections::BTreeSet<usize>> = answer.ok().map(|v| {
        v["indicesToRemove"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_u64())
            .map(|x| x as usize)
            .filter(|&i| i < n)
            .collect()
    });
    match indices {
        Some(set) if set.len() == to_remove => comments
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !set.contains(i))
            .map(|(_, c)| c)
            .collect(),
        _ => {
            tracing::info!(
                "pruning answer unusable; keeping the {STRICT_MAX_COMMENTS} most severe comments"
            );
            severity_fallback(comments, STRICT_MAX_COMMENTS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::Severity;

    #[test]
    fn fallback_keeps_most_severe_in_order() {
        let c = |sev: Severity, broad: bool, t: &str| Comment {
            agent: "A".into(),
            severity: sev,
            text: t.into(),
            quote: String::new(),
            file: "f".into(),
            range: (0, 1),
            broad,
        };
        let kept = severity_fallback(
            vec![
                c(Severity::Suggestion, false, "a"),
                c(Severity::Critical, true, "b"),
                c(Severity::Warning, true, "c"),
                c(Severity::Warning, false, "d"),
            ],
            2,
        );
        let t: Vec<&str> = kept.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(t, ["b", "d"]);
    }
}
