//! The set of reviewers a review runs. Prompt-driven reviewers are data (one table row each);
//! other kinds of reviewers are added to [`build`].

use super::{
    Reviewer,
    llm_reviewer::{Condition, LlmReviewer, Scope},
};
use crate::config::Config;
use std::sync::Arc;

fn llm(id: &'static str, name: &'static str, scope: Scope, max_comments: usize) -> LlmReviewer {
    LlmReviewer {
        id,
        name,
        prompt: id,
        scope,
        condition: Condition::Always,
        max_comments,
    }
}

/// The prompt-driven reviewers, in display order.
pub fn llm_reviewers() -> Vec<LlmReviewer> {
    use Scope::*;
    vec![
        llm("abstract", "Abstract", Abstract, 4),
        llm(
            "introduction",
            "Introduction",
            Categories(&["introduction"]),
            6,
        ),
        llm(
            "related_work",
            "Related work",
            Categories(&["related_work", "background"]),
            5,
        ),
        llm("methods", "Methods", Categories(&["methods"]), 8),
        llm(
            "results",
            "Results",
            Categories(&["experiments", "results", "discussion"]),
            8,
        ),
        llm("conclusion", "Conclusion", Categories(&["conclusion"]), 3),
        llm("appendix", "Appendix", Categories(&["appendix"]), 4),
        llm("writing_style", "Writing", Whole, 8),
        llm("latex_formatting", "LaTeX", Whole, 6),
        llm("figures_tables", "Figures and tables", Whole, 5),
        LlmReviewer {
            condition: Condition::VenueGiven,
            ..llm("venue", "Venue", Whole, 5)
        },
    ]
}

/// All reviewers of a full review.
pub fn build(_cfg: &Config) -> Vec<Arc<dyn Reviewer>> {
    llm_reviewers()
        .into_iter()
        .map(|r| Arc::new(r) as Arc<dyn Reviewer>)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::classify::CATEGORIES;

    #[test]
    fn table_is_consistent() {
        let prompts = crate::prompts::Prompts::load(None);
        let mut ids = std::collections::HashSet::new();
        for r in llm_reviewers() {
            assert!(ids.insert(r.id), "duplicate reviewer id {}", r.id);
            assert!(
                !prompts.get(r.prompt).is_empty(),
                "missing prompt {}",
                r.prompt
            );
            if let Scope::Categories(cats) = r.scope {
                for c in cats {
                    assert!(CATEGORIES.contains(c), "{} uses unknown category {c}", r.id);
                }
            }
        }
    }
}
