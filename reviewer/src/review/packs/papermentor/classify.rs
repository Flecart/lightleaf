//! The classifier call and upstream's rules for turning its answer into reviewer inputs:
//! single-category sections go to that category's reviewer; two-category sections are
//! grouped into one hybrid reviewer per category pair; appendix sections always go to the
//! appendix reviewer; sections the model skipped fall back to a title-keyword rule.

use super::{PaperMentor, sections::Section};
use crate::{
    llm::Request,
    review::{Ctx, template::render},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const PAPER_TYPES: &[&str] = &[
    "dataset",
    "method_improvement",
    "llm_engineering",
    "llm_inference_findings",
    "css",
    "position",
    "other",
];
pub const CATEGORIES: &[&str] = &[
    "abstract",
    "introduction",
    "related_work",
    "methods",
    "results",
    "conclusion",
    "appendix",
];

#[derive(Debug, Default)]
pub struct Classification {
    pub paper_type: String,
    pub summary: String,
    /// Section titles per single category.
    pub single: BTreeMap<String, Vec<String>>,
    /// Section titles per sorted category pair.
    pub pairs: BTreeMap<(String, String), Vec<String>>,
    /// typeSpecificGuidance: abstractFocus, introductionFocus, methodsFocus, resultsFocus, overallNotes.
    pub guidance: BTreeMap<String, String>,
}

/// The upstream schema, without array-length constraints (not supported by every provider's
/// structured output); the 1–2 categories rule is enforced when reading the answer.
fn schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false,
        "required": ["paperType", "paperTypeSummary", "sectionAssignments", "typeSpecificGuidance"],
        "properties": {
            "paperType": { "type": "string", "enum": PAPER_TYPES },
            "paperTypeSummary": { "type": "string", "description": "One-sentence description of why this paper type was chosen" },
            "sectionAssignments": {
                "type": "array",
                "description": "An assignment for EVERY section in the outline. Each section title must appear exactly once. No section should be left unassigned \u{2014} every section gets at least one reviewer. The vast majority of sections should have exactly one category. Only use two categories for a section that truly cannot be separated into one concern.",
                "items": {
                    "type": "object", "additionalProperties": false, "required": ["sectionTitle", "categories"],
                    "properties": {
                        "sectionTitle": { "type": "string", "description": "The EXACT section title from the outline" },
                        "categories": {
                            "type": "array",
                            "items": { "type": "string", "enum": CATEGORIES },
                            "description": "Which reviewer(s) this section should be assigned to. STRONGLY prefer a SINGLE category \u{2014} most sections belong to exactly one. Use TWO categories only in rare cases where a section genuinely spans two concerns \u{2014} e.g., a section that presents both methodology AND experimental results together. Never assign more than 2 categories. When in doubt, pick the single best fit. abstract = abstract, introduction = introduction, related_work = related work / background / prior work, methods = methods / approach / methodology / dataset construction / task formulation / preliminaries / experimental setup / implementation details / evaluation protocol / datasets and baselines, results = results / experiments / analysis / discussion / RQ sections / evaluation (the findings themselves, NOT the setup), conclusion = conclusion / limitations / ethical considerations / acknowledgments, appendix = all the appendix sections"
                        }
                    }
                }
            },
            "typeSpecificGuidance": {
                "type": "object", "additionalProperties": false,
                "description": "Reviewing criteria generated specifically for this paper type, to be injected into each subagent",
                "required": ["abstractFocus", "introductionFocus", "methodsFocus", "resultsFocus", "overallNotes"],
                "properties": {
                    "abstractFocus": { "type": "string", "description": "Type-specific reviewing criteria for the abstract reviewer" },
                    "introductionFocus": { "type": "string" },
                    "methodsFocus": { "type": "string" },
                    "resultsFocus": { "type": "string" },
                    "overallNotes": { "type": "string", "description": "Any additional type-specific notes for all reviewers" }
                }
            }
        }
    })
}

/// The user message: outline, sections to assign, abstract and introduction.
fn user_message(pack: &PaperMentor, sections: &[Section]) -> String {
    let outline: Vec<String> = sections
        .iter()
        .filter(|s| s.level <= 2)
        .map(|s| {
            format!(
                "{}{}{}",
                "  ".repeat(s.level as usize),
                if s.in_appendix { "[APPENDIX] " } else { "" },
                s.title
            )
        })
        .collect();
    let numbered: Vec<String> = sections
        .iter()
        .filter(|s| (1..=2).contains(&s.level) && !s.in_appendix)
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s.title))
        .collect();
    let appendix_count = sections
        .iter()
        .filter(|s| (1..=2).contains(&s.level) && s.in_appendix)
        .count();
    let appendix_note = if appendix_count > 0 {
        render(
            pack.files.must("prompts/classifier.appendix_note.md"),
            &[("appendixCount", &appendix_count.to_string())],
        )
    } else {
        String::new()
    };
    let find = |pred: &dyn Fn(&Section) -> bool| {
        sections
            .iter()
            .find(|s| pred(s))
            .map(|s| s.content.clone())
            .unwrap_or_else(|| "(not found)".into())
    };
    let abstract_content = find(&|s| s.title.eq_ignore_ascii_case("abstract"));
    let intro_content = find(&|s| s.title.to_lowercase().starts_with("introduction"));
    render(
        pack.files.must("prompts/classifier.user.md"),
        &[
            ("sectionTitles", &outline.join("\n")),
            ("numberedSections", &numbered.join("\n")),
            ("appendixNote", &appendix_note),
            ("abstractContent", &abstract_content),
            ("introContent", &intro_content),
        ],
    )
}

/// Upstream's fallback for sections the model did not assign.
fn keyword_category(title: &str) -> &'static str {
    let t = title.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if has(&["introduction"]) {
        "introduction"
    } else if has(&["related", "background", "prior"]) {
        "related_work"
    } else if has(&[
        "method",
        "approach",
        "dataset",
        "preliminar",
        "formulation",
        "framework",
    ]) {
        "methods"
    } else if has(&["conclusion", "limitation", "ethic", "acknowledgment"]) {
        "conclusion"
    } else if has(&["appendix"]) {
        "appendix"
    } else {
        "results"
    }
}

/// Applies upstream's assignment rules to the model's answer.
pub fn interpret(v: &Value, sections: &[Section]) -> Classification {
    let mut c = Classification {
        paper_type: v["paperType"]
            .as_str()
            .filter(|t| PAPER_TYPES.contains(t))
            .unwrap_or("other")
            .to_string(),
        summary: v["paperTypeSummary"].as_str().unwrap_or("").to_string(),
        ..Default::default()
    };
    for key in [
        "abstractFocus",
        "introductionFocus",
        "methodsFocus",
        "resultsFocus",
        "overallNotes",
    ] {
        if let Some(g) = v["typeSpecificGuidance"][key]
            .as_str()
            .filter(|g| !g.trim().is_empty())
        {
            c.guidance.insert(key.into(), g.into());
        }
    }
    let in_appendix = |title: &str| {
        sections
            .iter()
            .any(|s| s.in_appendix && s.title.trim().eq_ignore_ascii_case(title.trim()))
    };
    let mut assigned: Vec<String> = Vec::new();
    for a in v["sectionAssignments"].as_array().into_iter().flatten() {
        let Some(title) = a["sectionTitle"].as_str() else {
            continue;
        };
        let mut cats: Vec<String> = a["categories"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str())
            .filter(|x| CATEGORIES.contains(x))
            .map(String::from)
            .collect();
        cats.dedup();
        cats.truncate(2);
        if cats.is_empty() {
            continue;
        }
        assigned.push(title.trim().to_lowercase());
        if in_appendix(title) {
            c.single
                .entry("appendix".into())
                .or_default()
                .push(title.into());
        } else if cats.len() == 2 {
            cats.sort();
            c.pairs
                .entry((cats[0].clone(), cats[1].clone()))
                .or_default()
                .push(title.into());
        } else {
            c.single
                .entry(cats[0].clone())
                .or_default()
                .push(title.into());
        }
    }
    // Appendix sections are assigned automatically (the model was told not to list them).
    for s in sections
        .iter()
        .filter(|s| (1..=2).contains(&s.level) && s.in_appendix)
    {
        if !assigned.contains(&s.title.trim().to_lowercase()) {
            c.single
                .entry("appendix".into())
                .or_default()
                .push(s.title.clone());
            assigned.push(s.title.trim().to_lowercase());
        }
    }
    // Titles mentioning "appendix" belong to the appendix reviewer whatever the model said.
    let mut moved = Vec::new();
    for (cat, titles) in c
        .single
        .iter_mut()
        .filter(|(k, _)| k.as_str() != "appendix")
    {
        let _ = cat;
        titles.retain(|t| {
            let keep = !t.to_lowercase().contains("appendix");
            if !keep {
                moved.push(t.clone());
            }
            keep
        });
    }
    c.single.entry("appendix".into()).or_default().extend(moved);
    if sections.iter().any(|s| s.level == 0) {
        c.single
            .entry("abstract".into())
            .or_default()
            .push("Abstract".into());
    }
    // Sections the model skipped (including level-3 ones it never saw).
    for s in sections.iter().filter(|s| s.level > 0) {
        if !assigned.contains(&s.title.trim().to_lowercase()) {
            let cat = if s.in_appendix {
                "appendix"
            } else {
                keyword_category(&s.title)
            };
            c.single
                .entry(cat.into())
                .or_default()
                .push(s.title.clone());
        }
    }
    c.single.retain(|_, v| !v.is_empty());
    c
}

pub async fn run(
    pack: &PaperMentor,
    ctx: &Ctx,
    sections: &[Section],
) -> anyhow::Result<Classification> {
    let system = render(
        pack.files.must("prompts/classifier.system.md"),
        &[(
            "paperTypeDefinitions",
            &pack.skill("01_setup/paper_type_definitions.md"),
        )],
    );
    let task = user_message(pack, sections);
    let schema = schema();
    let v = ctx
        .llm
        .json(
            &Request {
                system: &system,
                document: "",
                task: &task,
                schema: &schema,
                max_tokens: 8000,
            },
            &ctx.usage,
        )
        .await?;
    Ok(interpret(&v, sections))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::packs::papermentor::sections::parse;

    #[test]
    fn applies_assignment_rules() {
        let text = "\\begin{abstract}\nA.\n\\end{abstract}\n\\section{Introduction}\nI.\n\\section{Approach and Results}\nX.\n\\section{Appendix-like Notes}\nN.\n\\subsubsection{Ablations}\nAb.\n\\appendix\n\\section{Proofs}\nP.\n";
        let s = parse(text);
        let v = json!({
            "paperType": "method_improvement", "paperTypeSummary": "s",
            "sectionAssignments": [
                {"sectionTitle": "Introduction", "categories": ["introduction"]},
                {"sectionTitle": "Approach and Results", "categories": ["results", "methods", "abstract"]},
                {"sectionTitle": "Appendix-like Notes", "categories": ["results"]}
            ],
            "typeSpecificGuidance": {"abstractFocus": "af", "introductionFocus": "", "methodsFocus": "mf", "resultsFocus": "rf", "overallNotes": "on"}
        });
        let c = interpret(&v, &s);
        assert_eq!(c.paper_type, "method_improvement");
        assert_eq!(c.single["introduction"], ["Introduction"]);
        assert_eq!(
            c.pairs[&("methods".to_string(), "results".to_string())],
            ["Approach and Results"]
        );
        assert!(c.single["appendix"].contains(&"Proofs".to_string()));
        assert!(c.single["appendix"].contains(&"Appendix-like Notes".to_string()));
        assert_eq!(c.single["abstract"], ["Abstract"]);
        assert_eq!(c.single["results"], ["Ablations"]);
        assert!(!c.guidance.contains_key("introductionFocus"));
    }
}
