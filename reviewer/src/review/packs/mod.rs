//! Review packs. Each pack is a self-contained methodology with its own prompts; the data files
//! live in `reviewer/packs/<id>/` and are compiled into the binary.
//!
//! - [`papermentor`] (default): the PaperMentor / AI Tutor pipeline, with its prompts and
//!   writing guides (third-party, AGPL-3.0; see `packs/papermentor/NOTICE.md`).
//! - [`lightleaf`]: a compact reviewer set with Lightleaf's own prompts.

pub mod lightleaf;
pub mod papermentor;

use super::Pack;
use std::{path::Path, sync::Arc};

pub const DEFAULT: &str = "papermentor";

/// The pack named `id`, with prompt overrides from `prompts_dir` (files with the same relative
/// path replace the built-in ones). `None` for an unknown id.
pub fn by_id(id: &str, prompts_dir: Option<&Path>) -> Option<Arc<dyn Pack>> {
    match id {
        "papermentor" => Some(Arc::new(papermentor::PaperMentor::new(Files::new(
            papermentor::files::FILES,
            prompts_dir,
        )))),
        "lightleaf" => Some(Arc::new(lightleaf::Lightleaf::new(Files::new(
            lightleaf::FILES,
            prompts_dir,
        )))),
        _ => None,
    }
}

/// The pack selected by the configuration, with its switches applied.
pub fn with_settings(cfg: &crate::config::Config) -> anyhow::Result<Arc<dyn Pack>> {
    let dir = cfg.prompts_dir.as_deref();
    Ok(match cfg.pack.as_str() {
        "papermentor" => Arc::new(
            papermentor::PaperMentor::new(Files::new(papermentor::files::FILES, dir))
                .with_settings(papermentor::Settings {
                    skip_anonymity: cfg.skip_anonymity,
                    skip_source_comments: cfg.skip_source_comments,
                    content_focus: cfg.content_focus,
                }),
        ),
        other => {
            by_id(other, dir).ok_or_else(|| anyhow::anyhow!("unknown review pack {other:?}"))?
        }
    })
}

pub fn ids() -> &'static [&'static str] {
    &["papermentor", "lightleaf"]
}

/// A pack's text files: built in, optionally overridden from a directory.
pub struct Files {
    builtin: &'static [(&'static str, &'static str)],
    overrides: std::collections::HashMap<String, String>,
}

impl Files {
    pub fn new(builtin: &'static [(&'static str, &'static str)], dir: Option<&Path>) -> Self {
        let mut overrides = std::collections::HashMap::new();
        if let Some(dir) = dir {
            for (name, _) in builtin {
                if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
                    tracing::info!("using custom pack file {name} from {}", dir.display());
                    overrides.insert(name.to_string(), text);
                }
            }
        }
        Files { builtin, overrides }
    }

    /// The file's text, or `None` if the pack has no such file.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.overrides.get(name).map(String::as_str).or_else(|| {
            self.builtin
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, t)| *t)
        })
    }

    /// Names of the built-in files.
    pub fn builtin_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.builtin.iter().map(|(n, _)| *n)
    }

    /// The file's text; panics in tests and logs in production if a required file is missing.
    pub fn must(&self, name: &str) -> &str {
        self.get(name).unwrap_or_else(|| {
            debug_assert!(false, "pack file {name} is missing");
            tracing::error!("pack file {name} is missing");
            ""
        })
    }
}
