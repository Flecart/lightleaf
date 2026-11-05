//! Overleaf-compatible zip import and export.
//!
//! Overleaf's "Download → Source" produces a zip with the project files at the archive root
//! (no wrapping folder, no build output). Its importer accepts that layout as well as zips
//! that wrap everything in a single top-level folder, and it picks the root document by looking
//! for a `.tex` file containing `\documentclass`.
//!
//! Lightleaf produces the same layout on export and, on import, mirrors Overleaf's root
//! document detection. Overleaf does not store the compiler in its zips, so the TeX engine is
//! inferred from the sources: magic comments, `latexmkrc`, and the packages the document loads.

use crate::util::clean_rel_path;
use serde::Serialize;
use std::{
    io::{Read, Write},
    path::Path,
};

/// Maximum uncompressed size and entry count accepted on import (zip-bomb protection).
const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

/// Files that are pure build output or OS junk. They are dropped on import because stale
/// copies would only confuse the next compile. `.bbl` is kept on purpose (arXiv-style
/// projects rely on it), as are PDFs (often figures).
fn is_junk(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if rel.starts_with("__MACOSX/") || rel.contains("/__MACOSX/") || name.starts_with("._") {
        return true;
    }
    if matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini") {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    [
        ".aux",
        ".log",
        ".fls",
        ".fdb_latexmk",
        ".synctex.gz",
        ".synctex",
        ".synctex(busy)",
        ".xdv",
        ".blg",
        ".run.xml",
        ".bcf",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

fn zip_time(meta: &std::fs::Metadata) -> Option<zip::DateTime> {
    let t = time::OffsetDateTime::from(meta.modified().ok()?);
    zip::DateTime::from_date_and_time(
        t.year().try_into().ok()?,
        t.month() as u8,
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
    )
    .ok()
}

/// Builds an Overleaf-style source zip: files at the root, directories preserved,
/// real modification times, `.git` excluded. `skip` lets the caller leave out files
/// Lightleaf generated itself (such as its default `.gitignore`).
pub fn export_zip(root: &Path, skip: impl Fn(&str, &[u8]) -> bool) -> anyhow::Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let base = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (rel, full, meta) in crate::files::walk(root) {
            let opts = match zip_time(&meta) {
                Some(t) => base.last_modified_time(t),
                None => base,
            };
            if meta.is_dir() {
                zw.add_directory(format!("{rel}/"), opts.unix_permissions(0o755))?;
                continue;
            }
            let data = std::fs::read(&full)?;
            if skip(&rel, &data) {
                continue;
            }
            zw.start_file(rel, opts)?;
            zw.write_all(&data)?;
        }
        zw.finish()?;
    }
    Ok(buf.into_inner())
}

#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub files: usize,
    pub skipped: Vec<String>,
}

/// Normalises an archive entry name (Windows separators, leading `./`).
fn entry_name(raw: &str) -> String {
    raw.replace('\\', "/").trim_start_matches("./").to_string()
}

/// Extracts an Overleaf (or any) project zip into `root`.
pub fn import_zip(bytes: &[u8], root: &Path) -> anyhow::Result<ImportReport> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
    if archive.len() > MAX_ENTRIES {
        anyhow::bail!(
            "archive has too many entries ({} > {MAX_ENTRIES})",
            archive.len()
        );
    }
    let names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            archive
                .by_index(i)
                .ok()
                .and_then(|f| f.name().ok().map(|n| entry_name(&n)))
        })
        .collect();

    // Strip one wrapping folder if every meaningful entry lives inside it.
    let meaningful: Vec<&String> = names
        .iter()
        .filter(|n| !is_junk(n) && !n.is_empty())
        .collect();
    let prefix = meaningful
        .first()
        .and_then(|n| n.split_once('/').map(|(top, _)| format!("{top}/")))
        .filter(|p| meaningful.iter().all(|n| n.starts_with(p.as_str())));

    let mut report = ImportReport::default();
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let mut f = archive.by_index(i)?;
        let Ok(raw) = f.name().map(|n| entry_name(&n)) else {
            continue;
        };
        let name = match &prefix {
            Some(p) => raw.strip_prefix(p.as_str()).unwrap_or(&raw).to_string(),
            None => raw.clone(),
        };
        if name.is_empty() {
            continue;
        }
        if is_junk(&name) || name.split('/').any(|c| c == ".git") {
            report.skipped.push(name);
            continue;
        }
        // Rejects absolute paths and `..` (zip-slip).
        let Ok(rel) = clean_rel_path(&name) else {
            report.skipped.push(name);
            continue;
        };
        let full = root.join(&rel);
        if f.is_dir() {
            std::fs::create_dir_all(&full)?;
            continue;
        }
        if f.is_symlink() {
            report.skipped.push(rel);
            continue;
        }
        total += f.size();
        if total > MAX_TOTAL_BYTES {
            anyhow::bail!(
                "archive is larger than {} MB uncompressed",
                MAX_TOTAL_BYTES / 1024 / 1024
            );
        }
        if let Some(p) = full.parent() {
            std::fs::create_dir_all(p)?;
        }
        // Read through a hard limit so a lying size header cannot exhaust the disk.
        let mut out = std::fs::File::create(&full)?;
        let copied = std::io::copy(&mut (&mut f).take(MAX_TOTAL_BYTES + 1), &mut out)?;
        if copied > MAX_TOTAL_BYTES {
            anyhow::bail!("archive entry too large");
        }
        report.files += 1;
    }
    if report.files == 0 {
        anyhow::bail!("the archive contains no files");
    }
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// Root document & engine detection
// ---------------------------------------------------------------------------------------------

/// Removes TeX comments (unescaped `%` to end of line).
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|l| {
            let b = l.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == b'%' {
                    return &l[..i];
                }
                i += 1;
            }
            l
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn document_class(src: &str) -> Option<String> {
    let code = strip_comments(src);
    let idx = code
        .find("\\documentclass")
        .or_else(|| code.find("\\documentstyle"))?;
    let rest = &code[idx..];
    let open = rest.find('{')?;
    let close = rest[open..].find('}')?;
    Some(rest[open + 1..open + close].trim().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detected {
    pub main_file: String,
    /// The engine the sources ask for.
    pub wanted: String,
    /// Why that engine was chosen (shown to the user).
    pub reason: String,
}

/// Picks the root document like Overleaf: a `.tex` file with `\documentclass`, preferring
/// `main.tex`, shallow paths, and real documents over `subfiles`/`standalone` pieces.
/// Falls back to Typst when there is no LaTeX root document.
pub fn detect(root: &Path) -> Detected {
    let files: Vec<String> = crate::files::walk(root)
        .into_iter()
        .filter(|e| e.2.is_file())
        .map(|e| e.0)
        .collect();
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap_or_default();

    let mut tex_roots: Vec<(String, String)> = files
        .iter()
        .filter(|f| f.ends_with(".tex") || f.ends_with(".ltx"))
        .filter_map(|f| {
            let src = read(f);
            document_class(&src).map(|cls| (f.clone(), cls))
        })
        .collect();
    tex_roots.sort_by_key(|(f, cls)| {
        let name = f.rsplit('/').next().unwrap_or(f);
        (
            matches!(cls.as_str(), "subfiles" | "standalone"),
            name != "main.tex",
            f.matches('/').count(),
            f.clone(),
        )
    });

    if let Some((main, _)) = tex_roots.first() {
        let (wanted, reason) = detect_engine(root, main, &read(main));
        return Detected {
            main_file: main.clone(),
            wanted,
            reason,
        };
    }

    let mut typ: Vec<&String> = files.iter().filter(|f| f.ends_with(".typ")).collect();
    typ.sort_by_key(|f| {
        (
            f.rsplit('/').next() != Some("main.typ"),
            f.matches('/').count(),
            (*f).clone(),
        )
    });
    if let Some(main) = typ.first() {
        return Detected {
            main_file: (*main).clone(),
            wanted: "typst".into(),
            reason: "Typst project".into(),
        };
    }

    let any_tex = files.iter().find(|f| f.ends_with(".tex")).cloned();
    Detected {
        main_file: any_tex.unwrap_or_else(|| "main.tex".into()),
        wanted: "pdflatex".into(),
        reason: "no root document found".into(),
    }
}

fn detect_engine(root: &Path, main: &str, src: &str) -> (String, String) {
    // 1. Magic comment, e.g. `% !TEX program = xelatex` (TeXShop, TeXstudio, VS Code, ...).
    for line in src.lines().take(30) {
        let l = line.trim().to_ascii_lowercase();
        if l.starts_with('%')
            && l.contains("!tex")
            && l.contains("program")
            && let Some(v) = l.split('=').nth(1)
        {
            let v = v.trim();
            for e in ["xelatex", "lualatex", "pdflatex", "tectonic"] {
                if v.starts_with(e) {
                    return (e.into(), format!("`% !TEX program` comment in {main}"));
                }
            }
        }
    }
    // 2. latexmkrc in the project root (Overleaf honours this file too).
    for rc in ["latexmkrc", ".latexmkrc"] {
        if let Ok(conf) = std::fs::read_to_string(root.join(rc)) {
            let conf = conf
                .lines()
                .map(|l| l.split('#').next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n");
            let squashed: String = conf.chars().filter(|c| !c.is_whitespace()).collect();
            if squashed.contains("$pdf_mode=5")
                || squashed.contains("$pdflatex='xelatex")
                || squashed.contains("$pdflatex=\"xelatex")
            {
                return ("xelatex".into(), format!("{rc} selects XeLaTeX"));
            }
            if squashed.contains("$pdf_mode=4")
                || squashed.contains("$pdflatex='lualatex")
                || squashed.contains("$pdflatex=\"lualatex")
            {
                return ("lualatex".into(), format!("{rc} selects LuaLaTeX"));
            }
        }
    }
    // 3. Engine-specific packages.
    let code = strip_comments(src);
    let uses = |pkgs: &[&'static str]| loads_package(&code, pkgs);
    if let Some(p) = uses(&[
        "luacode",
        "luatexja",
        "luatexja-fontspec",
        "luaotfload",
        "luamplib",
        "lua-visual-debug",
        "lualatex-math",
    ]) {
        return (
            "lualatex".into(),
            format!("{main} loads the LuaTeX-only package `{p}`"),
        );
    }
    if let Some(p) = uses(&[
        "fontspec",
        "unicode-math",
        "polyglossia",
        "xeCJK",
        "xltxtra",
        "xunicode",
        "mathspec",
        "xepersian",
        "bidi",
    ]) {
        return (
            "xelatex".into(),
            format!("{main} loads `{p}`, which needs XeLaTeX or LuaLaTeX"),
        );
    }
    if code.contains("\\documentclass")
        && document_class(src).is_some_and(|c| c.starts_with("ctex") || c == "xeCJK")
    {
        return ("xelatex".into(), format!("{main} uses a CJK class"));
    }
    ("folio".into(), "default engine".into())
}

/// Returns the first of `pkgs` that the (comment-free) source loads.
fn loads_package(code: &str, pkgs: &[&'static str]) -> Option<&'static str> {
    let loaded: Vec<&str> = code
        .match_indices("\\usepackage")
        .chain(code.match_indices("\\RequirePackage"))
        .flat_map(|(i, _)| {
            let rest = &code[i..];
            let end = rest.find('}').unwrap_or(rest.len()).min(400);
            rest[..end]
                .split(['{', ','])
                .skip(1)
                .map(str::trim)
                .collect::<Vec<_>>()
        })
        .collect();
    pkgs.iter().copied().find(|p| loaded.contains(p))
}

/// Maps the wanted engine onto what this server has installed. Lightleaf runs folio and
/// pdflatex for LaTeX, so a document that asks for another engine (XeLaTeX, LuaLaTeX, ...)
/// gets folio, or pdflatex when folio is missing; the caller tells the user about the switch.
pub fn choose_compiler(wanted: &str, installed: &[&str]) -> String {
    let has = |c: &str| installed.contains(&c);
    if has(wanted) {
        return wanted.to_string();
    }
    let fallbacks: &[&str] = match wanted {
        "typst" => &["typst"],
        _ => &["folio", "pdflatex"],
    };
    fallbacks
        .iter()
        .find(|c| has(c))
        .map(|c| c.to_string())
        .unwrap_or_else(|| wanted.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ll-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            for (n, c) in entries {
                if n.ends_with('/') {
                    zw.add_directory(*n, o).unwrap();
                } else {
                    zw.start_file(*n, o).unwrap();
                    zw.write_all(c.as_bytes()).unwrap();
                }
            }
            zw.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn imports_overleaf_flat_zip_and_detects_root() {
        let d = tmp();
        let z = make_zip(&[
            (
                "chapters/intro.tex",
                "\\documentclass[../main.tex]{subfiles}\n\\begin{document}x\\end{document}",
            ),
            (
                "main.tex",
                "\\documentclass{article}\n\\usepackage{fontspec}\n\\begin{document}\\end{document}",
            ),
            ("refs.bib", "@article{a, title={x}}"),
            ("main.aux", "junk"),
            ("__MACOSX/._main.tex", "junk"),
            ("figures/", ""),
        ]);
        let r = import_zip(&z, &d).unwrap();
        assert_eq!(r.files, 3);
        assert!(d.join("figures").is_dir());
        assert!(!d.join("main.aux").exists());
        let det = detect(&d);
        assert_eq!(det.main_file, "main.tex");
        assert_eq!(det.wanted, "xelatex");
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn strips_wrapping_folder_and_blocks_traversal() {
        let d = tmp();
        let z = make_zip(&[
            (
                "My_Paper/paper.tex",
                "% \\documentclass{fake}\n\\documentclass{article}\\begin{document}\\end{document}",
            ),
            ("My_Paper/img\\fig.png", "png"),
            ("My_Paper/.DS_Store", ""),
            ("My_Paper/../../evil.tex", "x"),
        ]);
        let r = import_zip(&z, &d).unwrap();
        assert!(d.join("paper.tex").exists());
        assert!(
            d.join("img/fig.png").exists(),
            "windows separators are normalised"
        );
        assert!(!d.parent().unwrap().join("evil.tex").exists());
        assert_eq!(r.files, 2);
        assert_eq!(detect(&d).main_file, "paper.tex");
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn engine_detection_sources() {
        let d = tmp();
        std::fs::write(
            d.join("main.tex"),
            "\\documentclass{article}\\begin{document}\\end{document}",
        )
        .unwrap();
        assert_eq!(detect(&d).wanted, "folio");
        std::fs::write(d.join("latexmkrc"), "$pdf_mode = 4; # lualatex\n").unwrap();
        assert_eq!(detect(&d).wanted, "lualatex");
        std::fs::write(
            d.join("main.tex"),
            "% !TEX program = xelatex\n\\documentclass{article}",
        )
        .unwrap();
        assert_eq!(detect(&d).wanted, "xelatex");
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn typst_fallback_and_compiler_choice() {
        let d = tmp();
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/main.typ"), "= Hi").unwrap();
        let det = detect(&d);
        assert_eq!(
            (det.main_file.as_str(), det.wanted.as_str()),
            ("src/main.typ", "typst")
        );
        assert_eq!(choose_compiler("xelatex", &["pdflatex", "folio"]), "folio");
        assert_eq!(choose_compiler("lualatex", &["pdflatex"]), "pdflatex");
        assert_eq!(choose_compiler("folio", &["pdflatex"]), "pdflatex");
        assert_eq!(choose_compiler("pdflatex", &["pdflatex"]), "pdflatex");
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn export_roundtrip_is_flat() {
        let d = tmp();
        std::fs::create_dir_all(d.join("sec")).unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join(".git/config"), "x").unwrap();
        std::fs::write(d.join("main.tex"), "\\documentclass{article}").unwrap();
        std::fs::write(d.join("sec/a.tex"), "A").unwrap();
        std::fs::write(d.join(".gitignore"), "generated").unwrap();
        let z = export_zip(&d, |rel, _| rel == ".gitignore").unwrap();
        let mut a = zip::ZipArchive::new(std::io::Cursor::new(z)).unwrap();
        let mut names: Vec<String> = (0..a.len())
            .map(|i| a.by_index(i).unwrap().name().unwrap().to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["main.tex", "sec/", "sec/a.tex"]);
        std::fs::remove_dir_all(d).ok();
    }
}
