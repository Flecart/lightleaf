//! Reviewer prompts. They are compiled into the binary from `reviewer/prompts/*.md`; any file
//! of the same name in `LIGHTLEAF_REVIEW_PROMPTS_DIR` replaces the built-in version, so a
//! deployment can tune its rubrics without rebuilding.

use std::{collections::HashMap, path::Path};

const BUILTIN: &[(&str, &str)] = &[
    ("system", include_str!("../prompts/system.md")),
    ("classify", include_str!("../prompts/classify.md")),
    ("abstract", include_str!("../prompts/abstract.md")),
    ("introduction", include_str!("../prompts/introduction.md")),
    ("related_work", include_str!("../prompts/related_work.md")),
    ("methods", include_str!("../prompts/methods.md")),
    ("results", include_str!("../prompts/results.md")),
    ("conclusion", include_str!("../prompts/conclusion.md")),
    ("appendix", include_str!("../prompts/appendix.md")),
    ("writing_style", include_str!("../prompts/writing_style.md")),
    (
        "latex_formatting",
        include_str!("../prompts/latex_formatting.md"),
    ),
    (
        "figures_tables",
        include_str!("../prompts/figures_tables.md"),
    ),
    ("venue", include_str!("../prompts/venue.md")),
];

pub struct Prompts(HashMap<String, String>);

impl Prompts {
    pub fn load(dir: Option<&Path>) -> Self {
        let mut map: HashMap<String, String> = BUILTIN
            .iter()
            .map(|(k, v)| (k.to_string(), v.trim().to_string()))
            .collect();
        if let Some(dir) = dir {
            for (name, _) in BUILTIN {
                if let Ok(text) = std::fs::read_to_string(dir.join(format!("{name}.md"))) {
                    tracing::info!("using custom prompt {name}.md from {}", dir.display());
                    map.insert(name.to_string(), text.trim().to_string());
                }
            }
        }
        Prompts(map)
    }

    pub fn get(&self, name: &str) -> String {
        self.0.get(name).cloned().unwrap_or_default()
    }
}
