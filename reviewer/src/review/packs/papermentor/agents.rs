//! The reviewer agents (upstream `SUBAGENT_DEFS` and the dynamic ones) and how their system
//! prompts are assembled from the pack files.

use super::PaperMentor;
use crate::review::{Options, template::render};

/// What text an agent reviews.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Input {
    /// The sections classified into the agent's category.
    Category(&'static str),
    /// The whole inlined document.
    Whole,
    /// Figure and table environments with context.
    Figures,
}

pub struct Def {
    pub id: &'static str,
    pub name: &'static str,
    pub input: Input,
    pub skills: &'static [&'static str],
    /// Key of the classifier's typeSpecificGuidance injected as "Type-Specific Review Focus".
    pub guidance: Option<&'static str>,
}

pub const STATIC: &[Def] = &[
    Def {
        id: "abstract",
        name: "Abstract Reviewer",
        input: Input::Category("abstract"),
        skills: &["04_paper_sections/abstract.md"],
        guidance: Some("abstractFocus"),
    },
    Def {
        id: "introduction",
        name: "Introduction Reviewer",
        input: Input::Category("introduction"),
        skills: &["04_paper_sections/introduction.md"],
        guidance: Some("introductionFocus"),
    },
    Def {
        id: "related_work",
        name: "Related Work Reviewer",
        input: Input::Category("related_work"),
        skills: &[
            "04_paper_sections/related_work.md",
            "06_writing_style/citations_and_references.md",
        ],
        guidance: None,
    },
    Def {
        id: "methods",
        name: "Methods Reviewer",
        input: Input::Category("methods"),
        skills: &[
            "04_paper_sections/methods.md",
            "04_paper_sections/task_formulation.md",
            "06_writing_style/math_and_formulas.md",
        ],
        guidance: Some("methodsFocus"),
    },
    Def {
        id: "results",
        name: "Results Reviewer",
        input: Input::Category("results"),
        skills: &["04_paper_sections/results_and_analysis.md"],
        guidance: Some("resultsFocus"),
    },
    Def {
        id: "conclusion",
        name: "Conclusion & Supplements Reviewer",
        input: Input::Category("conclusion"),
        skills: &[
            "04_paper_sections/conclusion.md",
            "04_paper_sections/limitations.md",
            "04_paper_sections/ethical_considerations.md",
        ],
        guidance: None,
    },
    Def {
        id: "appendix",
        name: "Appendix Reviewer",
        input: Input::Category("appendix"),
        skills: &["04_paper_sections/faq_appendix.md"],
        guidance: None,
    },
    Def {
        id: "writing_style",
        name: "Writing Style Reviewer",
        input: Input::Whole,
        skills: &[
            "06_writing_style/grammar_and_punctuation.md",
            "06_writing_style/capitalization_and_acronyms.md",
            "06_writing_style/general_writing_habits.md",
            "06_writing_style/citations_and_references.md",
        ],
        guidance: None,
    },
    Def {
        id: "latex_formatting",
        name: "LaTeX & Formatting Reviewer",
        input: Input::Whole,
        skills: &[
            "06_writing_style/latex_formatting.md",
            "06_writing_style/math_and_formulas.md",
            "05_figures_and_tables/table_formatting.md",
            "04_paper_sections/front_matter.md",
        ],
        guidance: None,
    },
    Def {
        id: "figures_tables",
        name: "Figures & Captions Reviewer",
        input: Input::Figures,
        skills: &[
            "05_figures_and_tables/caption_writing.md",
            "05_figures_and_tables/figure1_design.md",
            "05_figures_and_tables/experiment_visualization.md",
        ],
        guidance: None,
    },
];

/// Upstream treats these as whole-document agents in dedup tie-breaks.
pub fn broad(id: &str) -> bool {
    matches!(id, "writing_style" | "latex_formatting" | "figures_tables")
}

/// Comments per agent the prompt asks for (upstream: 10).
pub const MAX_COMMENTS: usize = 10;

/// Venues reviewed under ACL Rolling Review rules.
fn is_arr(venue: &str) -> bool {
    ["acl", "emnlp", "naacl", "eacl"]
        .iter()
        .any(|p| venue == *p || venue.starts_with(&format!("{p}_")))
}

impl PaperMentor {
    /// An agent's preamble (`subagent.<id>.preamble.md`), with the conclusion venue line.
    pub fn preamble(&self, id: &str, venue: &str) -> String {
        let p = self
            .files
            .must(&format!("prompts/subagent.{id}.preamble.md"))
            .to_string();
        if id == "conclusion" {
            let file = if is_arr(venue) {
                "subagent.conclusion.venue_line.arr.md"
            } else {
                "subagent.conclusion.venue_line.non_arr.md"
            };
            return format!(
                "{p}\n{}",
                render(
                    self.files.must(&format!("prompts/{file}")),
                    &[("venue", venue)]
                )
            );
        }
        p
    }

    /// The preamble of a hybrid agent combining `members` (their original preambles).
    pub fn hybrid_preamble(&self, categories: &[&str], members: &[&Def]) -> String {
        let parts: Vec<String> = members
            .iter()
            .map(|m| {
                render(
                    self.files.must("prompts/subagent.hybrid.preamble_part.md"),
                    &[
                        ("memberAgentName", m.name),
                        (
                            "memberSystemPreamble",
                            self.files
                                .must(&format!("prompts/subagent.{}.preamble.md", m.id)),
                        ),
                    ],
                )
            })
            .collect();
        render(
            self.files.must("prompts/subagent.hybrid.preamble.md"),
            &[
                ("categoriesCommaList", &categories.join(", ")),
                ("preambleParts", &parts.join("\n")),
            ],
        )
    }

    /// The full system prompt of a reviewer (upstream `reviewer.system.md` assembly).
    pub fn system_prompt(
        &self,
        agent_name: &str,
        preamble: &str,
        skills: &[&str],
        guidance_keys: &[&str],
        guidance: &std::collections::BTreeMap<String, String>,
        options: &Options,
    ) -> String {
        let f = |n: &str| self.files.must(&format!("prompts/{n}")).to_string();
        let default_instructions = render(
            &f(if options.strict {
                "reviewer.default_instructions.strict.md"
            } else {
                "reviewer.default_instructions.normal.md"
            }),
            &[("maxComments", &MAX_COMMENTS.to_string())],
        );
        let skill_content = skills
            .iter()
            .map(|s| {
                render(
                    &f("reviewer.skill_block.md"),
                    &[("skillPath", s), ("skillContent", &self.skill(s))],
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut injection = String::new();
        for key in guidance_keys {
            if let Some(v) = guidance.get(*key) {
                injection.push_str(&render(
                    &f("reviewer.guidance_injection.focus.md"),
                    &[("guidanceKey", key), ("guidanceValue", v)],
                ));
            }
        }
        if !guidance_keys.contains(&"overallNotes")
            && let Some(v) = guidance.get("overallNotes")
        {
            injection.push_str(&render(
                &f("reviewer.guidance_injection.overall_notes.md"),
                &[("overallNotes", v)],
            ));
        }
        let cfg = &self.settings;
        render(
            &f("reviewer.system.md"),
            &[
                ("agentName", agent_name),
                ("systemPreamble", preamble),
                (
                    "skipAnonymityLine",
                    &if cfg.skip_anonymity {
                        f("reviewer.skip_anonymity_line.md")
                    } else {
                        String::new()
                    },
                ),
                (
                    "skipSourceCommentsLine",
                    &if cfg.skip_source_comments {
                        f("reviewer.skip_source_comments_line.md")
                    } else {
                        String::new()
                    },
                ),
                ("defaultInstructions", &default_instructions),
                (
                    "strictBlock",
                    &if options.strict {
                        f("reviewer.strict_block.md")
                    } else {
                        String::new()
                    },
                ),
                ("EDITORIAL_REVIEW_CHECKS", &f("editorial_review_checks.md")),
                ("skillContent", &skill_content),
                ("guidanceInjection", &injection),
                ("roleModelInjection", ""),
            ],
        )
    }

    /// The user message of a reviewer.
    pub fn user_prompt(&self, review_text: &str) -> String {
        render(
            self.files.must("prompts/reviewer.user.md"),
            &[("reviewText", review_text)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arr_venues() {
        assert!(is_arr("acl_2026") && is_arr("emnlp_2025") && is_arr("naacl"));
        assert!(!is_arr("neurips_2025") && !is_arr("arxiv") && !is_arr("aclx"));
    }
}
