//! Applying comments to a checkout on disk, and removing them again.

use super::{PACKAGE, anchor, markup};
use std::{collections::BTreeMap, path::Path};

/// A comment to write: after `quote` in `file`, as `\<macro>[label]{text}`.
pub struct Annotation<'a> {
    pub file: &'a str,
    pub quote: &'a str,
    pub label: String,
    pub text: &'a str,
}

/// How many annotations were written and how many found no safe place.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub written: usize,
    pub skipped: usize,
}

/// Writes `notes` into the files under `dir`, re-anchoring each one in the file's current text
/// (it may have changed since the review). When anything was written, also makes sure the
/// project has `lightleaf-comments.sty` and the main file loads it.
pub fn apply(
    dir: &Path,
    main_file: &str,
    notes: &[Annotation],
    macro_name: &str,
) -> anyhow::Result<Applied> {
    let mut by_file: BTreeMap<&str, Vec<&Annotation>> = BTreeMap::new();
    for n in notes {
        by_file.entry(n.file).or_default().push(n);
    }
    let mut applied = Applied::default();
    for (file, list) in by_file {
        let path = dir.join(file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            applied.skipped += list.len();
            continue;
        };
        let mut inserts: Vec<(usize, String)> = Vec::new();
        for n in list {
            match anchor::find_quote(&text, n.quote)
                .and_then(|(_, end)| anchor::insertion_point(&text, end))
            {
                Some(p) => inserts.push((p, markup::comment_macro(macro_name, &n.label, n.text))),
                None => applied.skipped += 1,
            }
        }
        if inserts.is_empty() {
            continue;
        }
        applied.written += inserts.len();
        // Insert from the end so earlier offsets stay valid (stable for equal points).
        inserts.sort_by_key(|(p, _)| *p);
        let mut out = text;
        for (p, m) in inserts.into_iter().rev() {
            out.insert_str(p, &m);
        }
        std::fs::write(&path, out)?;
    }
    if applied.written > 0 {
        install_package(dir, main_file, macro_name)?;
    }
    Ok(applied)
}

/// Adds `lightleaf-comments.sty` (unless the project has its own) and loads it in the main file.
fn install_package(dir: &Path, main_file: &str, macro_name: &str) -> anyhow::Result<()> {
    let sty = dir.join(format!("{PACKAGE}.sty"));
    if !sty.exists() {
        std::fs::write(&sty, markup::package_source(macro_name))?;
    }
    let main_path = dir.join(main_file);
    let main = std::fs::read_to_string(&main_path)?;
    if let Some(updated) = markup::ensure_package(&main)? {
        std::fs::write(&main_path, updated)?;
    }
    Ok(())
}

/// Removes every review comment from all `.tex` files under `dir`. Returns how many.
pub fn clear(dir: &Path, macro_name: &str) -> anyhow::Result<usize> {
    let mut removed = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d)?.flatten() {
            let ft = e.file_type()?;
            if ft.is_symlink() || e.file_name() == ".git" {
                continue;
            }
            let p = e.path();
            if ft.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "tex")
                && let Ok(text) = std::fs::read_to_string(&p)
            {
                let (clean, n) = markup::remove_comments(&text, macro_name);
                if n > 0 {
                    std::fs::write(&p, clean)?;
                    removed += n;
                }
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_and_clears_comments_in_a_checkout() {
        let dir = std::env::temp_dir().join(format!("llr-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(dir.join("sec")).unwrap();
        std::fs::write(dir.join("main.tex"), "\\documentclass{article}\n\\begin{document}\nWe show that our method is always better.\n\\input{sec/a}\n\\end{document}\n").unwrap();
        std::fs::write(
            dir.join("sec/a.tex"),
            "Results are in Table 1 and look good.\n",
        )
        .unwrap();
        let notes = [
            Annotation {
                file: "main.tex",
                quote: "our method is always better",
                label: "Results, critical".into(),
                text: "\"Always\" is not supported by the experiments.",
            },
            Annotation {
                file: "sec/a.tex",
                quote: "Results are in Table 1",
                label: "Writing, suggestion".into(),
                text: "Say what the table shows.",
            },
            Annotation {
                file: "main.tex",
                quote: "not present anywhere in the files",
                label: "Writing, suggestion".into(),
                text: "x",
            },
        ];
        assert_eq!(
            apply(&dir, "main.tex", &notes, "AI").unwrap(),
            Applied {
                written: 2,
                skipped: 1
            }
        );
        let main = std::fs::read_to_string(dir.join("main.tex")).unwrap();
        assert!(main.contains("\\usepackage{lightleaf-comments}"));
        assert!(main.contains("always better. \\AI[Results, critical]{\"Always\" is not supported by the experiments.}\n"));
        assert!(dir.join("lightleaf-comments.sty").exists());
        assert_eq!(clear(&dir, "AI").unwrap(), 2);
        assert_eq!(
            std::fs::read_to_string(dir.join("sec/a.tex")).unwrap(),
            "Results are in Table 1 and look good.\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
