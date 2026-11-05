//! Citation keys used in the `.tex` sources, with where each is first cited.

use crate::latex::Document;
use std::collections::BTreeMap;

/// Where a key is first cited: the file and the whole `\cite…{…}` command text.
#[derive(Debug, Clone)]
pub struct Citation {
    pub file: String,
    pub command: String,
}

/// Commands that cite (natbib, biblatex and plain LaTeX).
fn is_cite_command(name: &str) -> bool {
    let n = name.trim_end_matches('*').to_lowercase();
    n.starts_with("cite")
        || n.ends_with("cite")
        || n.ends_with("cites")
        || matches!(n.as_str(), "nocite" | "fullcite" | "footcite")
}

/// Every cited key (except `*`), in order of first citation.
pub fn citations(doc: &Document) -> BTreeMap<String, Citation> {
    let mut out: BTreeMap<String, Citation> = BTreeMap::new();
    for f in &doc.files {
        for line in f.text.lines() {
            let code = &line[..crate::latex::comment_start(line).unwrap_or(line.len())];
            let b = code.as_bytes();
            let mut i = 0;
            while let Some(off) = code[i..].find('\\') {
                let start = i + off;
                let mut j = start + 1;
                while j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'*') {
                    j += 1;
                }
                let name = &code[start + 1..j];
                i = j.max(start + 1);
                if name.is_empty() || !is_cite_command(name) {
                    continue;
                }
                // Optional arguments, then one or more {keys} groups (biblatex \cites).
                let mut k = j;
                let mut end = j;
                let mut keys = Vec::new();
                loop {
                    while k < b.len() && b[k] == b' ' {
                        k += 1;
                    }
                    match b.get(k) {
                        Some(b'[') => match code[k..].find(']') {
                            Some(e) => {
                                k += e + 1;
                                end = k;
                            }
                            None => break,
                        },
                        Some(b'{') => match code[k..].find('}') {
                            Some(e) => {
                                keys.extend(
                                    code[k + 1..k + e]
                                        .split(',')
                                        .map(|s| s.trim().to_string())
                                        .filter(|s| !s.is_empty() && s != "*"),
                                );
                                k += e + 1;
                                end = k;
                            }
                            None => break,
                        },
                        _ => break,
                    }
                }
                let command = code[start..end].to_string();
                for key in keys {
                    out.entry(key).or_insert_with(|| Citation {
                        file: f.path.clone(),
                        command: command.clone(),
                    });
                }
                i = k.max(i);
            }
        }
    }
    out
}

/// `.bib` files named by `\bibliography{a,b}` / `\addbibresource{x.bib}` (outside comments).
pub fn bib_files(doc: &Document) -> Vec<String> {
    let mut out = Vec::new();
    for f in &doc.files {
        for line in f.text.lines() {
            let code = &line[..crate::latex::comment_start(line).unwrap_or(line.len())];
            for cmd in ["\\bibliography{", "\\addbibresource{", "\\addglobalbib{"] {
                let mut rest = code;
                while let Some(i) = rest.find(cmd) {
                    let after = &rest[i + cmd.len()..];
                    let Some(end) = after.find('}') else { break };
                    for name in after[..end]
                        .split(',')
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                    {
                        let p = if name.ends_with(".bib") {
                            name.to_string()
                        } else {
                            format!("{name}.bib")
                        };
                        if !p.contains("..") && !p.starts_with('/') && !out.contains(&p) {
                            out.push(p);
                        }
                    }
                    rest = &after[end..];
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::latex::document::{Document, SourceFile};

    #[test]
    fn finds_citations_and_bibliographies() {
        let doc = Document {
            files: vec![SourceFile {
                path: "main.tex".into(),
                text: "As shown by \\citet[p.~3]{smith2020, doe21} and \\parencite*{x}.\n% \\cite{commented}\n\\cites{a}{b} \\nocite{*}\n\\bibliography{refs,more.bib}\n\\addbibresource{extra.bib}".into(),
            }],
        };
        let c = citations(&doc);
        assert_eq!(
            c.keys().cloned().collect::<Vec<_>>(),
            ["a", "b", "doe21", "smith2020", "x"]
        );
        assert_eq!(c["smith2020"].command, "\\citet[p.~3]{smith2020, doe21}");
        assert_eq!(bib_files(&doc), ["refs.bib", "more.bib", "extra.bib"]);
    }
}
