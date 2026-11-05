//! The `papermentor` pack: a port of the PaperMentor / AI Tutor review pipeline
//! (https://github.com/jiarui-liu/overleaf, AGPL-3.0) with its prompts and writing guides,
//! which live unchanged in `reviewer/packs/papermentor/` (see its NOTICE.md).
//!
//! Per review: parse sections ([`sections`]); classify the paper type and every section
//! ([`classify`]); build one reviewer per category with sections ([`agents`]), one hybrid
//! reviewer per category pair, the whole-paper reviewers, a paper-type reviewer and a venue
//! reviewer when a guide exists, and the two rule-based checks ([`rules`]). In strict mode,
//! suggestions are dropped and at most 30 comments are kept ([`prune`]).
//!
//! Not ported: the Sentence Placement reviewer (needs third-party paper texts), the
//! Acknowledgments/Affiliations editors (one lab's house rules), role-model uploads.

pub mod agents;
mod classify;
pub mod files;
mod prune;
mod rules;
mod sections;

use super::Files;
use crate::{
    latex::Document,
    review::{BoxFuture, Comment, Ctx, Options, Pack, Plan, PromptReviewer, Reviewer},
};
use agents::{Def, Input, STATIC};
use serde_json::{Value, json};
use std::sync::Arc;

/// Upstream's `AI_TUTOR_SKIP_*` / `AI_TUTOR_CONTENT_FOCUS` switches.
#[derive(Default, Clone, Debug)]
pub struct Settings {
    pub skip_anonymity: bool,
    pub skip_source_comments: bool,
    pub content_focus: bool,
}

pub struct PaperMentor {
    pub files: Files,
    pub settings: Settings,
}

/// The upstream comment schema (field names and descriptions as upstream).
fn comments_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["comments"],
        "properties": { "comments": { "type": "array", "items": {
            "type": "object", "additionalProperties": false, "required": ["highlightText", "comment", "severity"],
            "properties": {
                "highlightText": { "type": "string", "description": "The EXACT text from the paper to highlight. Must be a verbatim substring (1-200 chars)." },
                "comment": { "type": "string", "description": "Constructive review comment or suggestion for this highlighted text" },
                "severity": { "type": "string", "enum": ["suggestion", "warning", "critical"], "description": "suggestion = nice-to-have, warning = should fix, critical = must fix" }
            }
        }}}
    })
}

/// "related_work" -> "Related work" (upstream: upper-case the first letter, first '_' to ' ').
fn pretty(category: &str) -> String {
    let s = category.replacen('_', " ", 1);
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

impl PaperMentor {
    pub fn new(files: Files) -> Self {
        PaperMentor {
            files,
            settings: Settings::default(),
        }
    }

    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
    }

    /// A writing guide, with `{{CURRENT_YEAR}}` / `{{CURRENT_DATE}}` filled in.
    pub fn skill(&self, path: &str) -> String {
        match self.files.get(&format!("skills/{path}")) {
            Some(t) => {
                let today = crate::jobs::today();
                t.replace("{{CURRENT_YEAR}}", &today[..4])
                    .replace("{{CURRENT_DATE}}", &today)
            }
            None => format!("[skill file not found: {path}]"),
        }
    }

    fn has_skill(&self, path: &str) -> bool {
        self.files.get(&format!("skills/{path}")).is_some()
    }

    fn reviewer(
        &self,
        id: String,
        name: String,
        system: String,
        text: String,
        broad: bool,
        schema: &Arc<Value>,
    ) -> Arc<dyn Reviewer> {
        Arc::new(PromptReviewer {
            id,
            name,
            system,
            document: Arc::new(String::new()),
            task: self.user_prompt(&text),
            schema: schema.clone(),
            quote_field: "highlightText",
            max_comments: agents::MAX_COMMENTS,
            broad,
        })
    }
}

impl Pack for PaperMentor {
    fn id(&self) -> &'static str {
        "papermentor"
    }

    fn venues(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .files
            .builtin_names()
            .filter_map(|n| {
                n.strip_prefix("skills/02_venues/")?
                    .strip_suffix(".md")
                    .map(String::from)
            })
            .filter(|id| id != "arxiv_default")
            .map(|id| {
                let label = id
                    .split('_')
                    .map(|p| {
                        if p.chars().all(|c| c.is_ascii_digit()) {
                            p.to_string()
                        } else {
                            p.to_uppercase()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                (id, label)
            })
            .collect();
        v.sort();
        v
    }

    fn plan<'a>(
        &'a self,
        ctx: &'a Ctx,
        doc: &'a Document,
        o: &'a Options,
    ) -> BoxFuture<'a, anyhow::Result<Plan>> {
        Box::pin(async move {
            let text = doc.inlined();
            let secs = sections::parse(&text);
            let class = classify::run(self, ctx, &secs).await?;
            let venue = if o.venue.trim().is_empty() {
                "arxiv".to_string()
            } else {
                o.venue.trim().to_lowercase()
            };
            let schema = Arc::new(comments_schema());
            let mut out: Vec<Arc<dyn Reviewer>> = Vec::new();
            let system = |name: &str, preamble: &str, skills: &[&str], keys: &[&str]| {
                self.system_prompt(name, preamble, skills, keys, &class.guidance, o)
            };

            // Section and whole-paper reviewers.
            let figures = sections::figures_and_tables(&text);
            for d in STATIC {
                let input = match d.input {
                    Input::Category(c) => sections::collect(
                        &secs,
                        class.single.get(c).map(Vec::as_slice).unwrap_or(&[]),
                    ),
                    Input::Whole => text.clone(),
                    Input::Figures => figures.clone(),
                };
                let min = match d.input {
                    Input::Figures => 50,
                    _ => 30,
                };
                if input.trim().len() < min {
                    continue;
                }
                let keys: Vec<&str> = d.guidance.into_iter().collect();
                let sys = system(d.name, &self.preamble(d.id, &venue), d.skills, &keys);
                out.push(self.reviewer(
                    d.id.into(),
                    d.name.into(),
                    sys,
                    input,
                    agents::broad(d.id),
                    &schema,
                ));
            }

            // Paper type reviewer, when the type has a guide.
            let type_skill = format!("03_paper_types/{}_paper.md", class.paper_type);
            if self.has_skill(&type_skill) {
                let preamble = crate::review::template::render(
                    self.files.must("prompts/subagent.paper_type.preamble.md"),
                    &[
                        ("paperType", &class.paper_type),
                        ("paperTypeSummary", &class.summary),
                    ],
                );
                let name = format!("Paper Type Reviewer ({})", class.paper_type);
                let sys = system(&name, &preamble, &[&type_skill], &["overallNotes"]);
                out.push(self.reviewer(
                    "paper_type".into(),
                    name,
                    sys,
                    text.clone(),
                    false,
                    &schema,
                ));
            }

            // Venue reviewer, when a venue other than arXiv has a guide.
            let venue_skill = format!("02_venues/{venue}.md");
            if venue != "arxiv" && self.has_skill(&venue_skill) {
                let preamble = crate::review::template::render(
                    self.files.must("prompts/subagent.venue.preamble.md"),
                    &[("venue", &venue)],
                );
                let name = format!("Venue Reviewer ({venue})");
                let sys = system(&name, &preamble, &[&venue_skill], &["overallNotes"]);
                out.push(self.reviewer("venue".into(), name, sys, text.clone(), false, &schema));
            }

            // One hybrid reviewer per category pair.
            for ((a, b), titles) in &class.pairs {
                let cats = [a.as_str(), b.as_str()];
                let members: Vec<&Def> = STATIC
                    .iter()
                    .filter(|d| matches!(d.input, Input::Category(c) if cats.contains(&c)))
                    .collect();
                let mut skills: Vec<&str> = Vec::new();
                for m in &members {
                    for s in m.skills {
                        if !skills.contains(s) {
                            skills.push(s);
                        }
                    }
                }
                let keys: Vec<&str> = members.iter().filter_map(|m| m.guidance).collect();
                let name = format!(
                    "{} Reviewer (hybrid)",
                    cats.iter()
                        .map(|c| pretty(c))
                        .collect::<Vec<_>>()
                        .join(" + ")
                );
                let sys = system(
                    &name,
                    &self.hybrid_preamble(&cats, &members),
                    &skills,
                    &keys,
                );
                let input = sections::collect(&secs, titles);
                if input.trim().len() >= 30 {
                    out.push(self.reviewer(
                        format!("hybrid_{a}+{b}"),
                        name,
                        sys,
                        input,
                        false,
                        &schema,
                    ));
                }
            }

            // Rule-based checks.
            if let Some(f) = rules::abstract_footnote(
                &secs,
                self.files.must("fixed/abstract_footnote.comment.txt"),
            ) {
                out.push(Arc::new(rules::RuleReviewer {
                    id: "abstract_footnote",
                    name: "Abstract Footnote Check",
                    findings: vec![f],
                }));
            }
            if class.paper_type != "position"
                && let Some(f) = rules::section_structure(
                    &secs,
                    self.files
                        .must("fixed/section_structure.no_setup.comment.txt"),
                    self.files
                        .must("fixed/section_structure.setup_after_results.comment.txt"),
                )
            {
                out.push(Arc::new(rules::RuleReviewer {
                    id: "section_structure",
                    name: "Section Structure Check",
                    findings: vec![f],
                }));
            }

            Ok(Plan {
                paper_type: class.paper_type,
                summary: class.summary,
                reviewers: out,
            })
        })
    }

    fn finalize<'a>(
        &'a self,
        ctx: &'a Ctx,
        comments: Vec<Comment>,
        o: &'a Options,
    ) -> BoxFuture<'a, Vec<Comment>> {
        Box::pin(async move {
            if o.strict {
                prune::prune(self, ctx, comments).await
            } else {
                comments
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::template::unresolved;
    use std::collections::BTreeMap;

    fn pack() -> PaperMentor {
        PaperMentor::new(Files::new(files::FILES, None))
    }

    #[test]
    fn lists_every_file() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packs/papermentor");
        let mut on_disk = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "md" || x == "txt")
                    && p.file_name().unwrap() != "NOTICE.md"
                {
                    on_disk.push(
                        p.strip_prefix(&root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
        on_disk.sort();
        let listed: Vec<String> = files::FILES.iter().map(|(n, _)| n.to_string()).collect();
        assert_eq!(
            on_disk, listed,
            "regenerate src/review/packs/papermentor/files.rs"
        );
    }

    #[test]
    fn every_agent_prompt_renders_completely() {
        let p = pack();
        let guidance: BTreeMap<String, String> = [
            "abstractFocus",
            "introductionFocus",
            "methodsFocus",
            "resultsFocus",
            "overallNotes",
        ]
        .iter()
        .map(|k| (k.to_string(), "g".to_string()))
        .collect();
        for strict in [false, true] {
            let o = Options {
                strict,
                ..Default::default()
            };
            for d in STATIC {
                for s in d.skills {
                    assert!(p.has_skill(s), "missing skill {s}");
                }
                let keys: Vec<&str> = d.guidance.into_iter().collect();
                let sys = p.system_prompt(
                    d.name,
                    &p.preamble(d.id, "acl_2026"),
                    d.skills,
                    &keys,
                    &guidance,
                    &o,
                );
                assert!(
                    unresolved(&sys).is_empty(),
                    "{}: unresolved {:?}",
                    d.id,
                    unresolved(&sys)
                );
                assert!(sys.contains(d.name) && sys.contains("## Writing Skills Reference"));
            }
        }
        let members: Vec<&Def> = STATIC
            .iter()
            .filter(|d| ["methods", "results"].contains(&d.id))
            .collect();
        let hybrid = p.hybrid_preamble(&["methods", "results"], &members);
        assert!(unresolved(&hybrid).is_empty() && hybrid.contains("[Methods Reviewer]"));
        assert!(
            p.preamble("conclusion", "acl_2026")
                .contains("ACL Rolling Review")
        );
        assert!(unresolved(&p.user_prompt("text")).is_empty());
        assert_eq!(pretty("related_work"), "Related work");
    }

    #[test]
    fn venues_come_from_the_guides() {
        let v = pack().venues();
        assert!(
            v.iter()
                .any(|(id, label)| id == "neurips_2025" && label == "NEURIPS 2025")
        );
        assert!(!v.iter().any(|(id, _)| id == "arxiv_default"));
    }
}
