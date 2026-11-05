//! A BibTeX parser that is good enough for real bibliographies: case-insensitive entry types,
//! `@string` macros and `#` concatenation, braced and quoted values with nesting, bare numbers,
//! and `%` comments between entries.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    /// Lowercased entry type (`article`, `inproceedings`, ...).
    pub kind: String,
    /// Lowercased field names; values with outer braces/quotes removed and macros expanded.
    pub fields: HashMap<String, String>,
    pub file: String,
}

impl Entry {
    pub fn field(&self, name: &str) -> &str {
        self.fields.get(name).map(String::as_str).unwrap_or("")
    }
}

fn month_macros() -> HashMap<String, String> {
    [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .map(|m| (m.to_string(), m.to_string()))
    .collect()
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    /// A `{…}` group (without the outer braces), honouring nesting and backslash escapes.
    fn braced(&mut self) -> Option<String> {
        let start = self.i + 1;
        let mut depth = 0;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'\\' => self.i += 1,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        self.i += 1;
                        return Some(
                            String::from_utf8_lossy(&self.s[start..self.i - 1]).into_owned(),
                        );
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        None
    }

    /// A `"…"` string (braces inside may contain quotes).
    fn quoted(&mut self) -> Option<String> {
        let start = self.i + 1;
        self.i += 1;
        let mut depth = 0;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'\\' => self.i += 1,
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b'"' if depth == 0 => {
                    self.i += 1;
                    return Some(String::from_utf8_lossy(&self.s[start..self.i - 1]).into_owned());
                }
                _ => {}
            }
            self.i += 1;
        }
        None
    }

    fn ident(&mut self) -> String {
        let start = self.i;
        while self.i < self.s.len()
            && !matches!(
                self.s[self.i],
                b'=' | b',' | b'#' | b'}' | b')' | b'{' | b'(' | b'"'
            )
            && !self.s[self.i].is_ascii_whitespace()
        {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
    }

    /// A value: parts joined by `#`, each braced, quoted, a number or a macro name.
    fn value(&mut self, strings: &HashMap<String, String>) -> Option<String> {
        let mut out = String::new();
        loop {
            self.skip_ws();
            let part = match self.s.get(self.i)? {
                b'{' => self.braced()?,
                b'"' => self.quoted()?,
                _ => {
                    let id = self.ident();
                    if id.is_empty() {
                        return None;
                    }
                    strings.get(&id.to_lowercase()).cloned().unwrap_or(id)
                }
            };
            out.push_str(&part);
            self.skip_ws();
            if self.s.get(self.i) == Some(&b'#') {
                self.i += 1;
            } else {
                return Some(out);
            }
        }
    }
}

/// Parses `text` (from `file`). Malformed entries are skipped.
pub fn parse(text: &str, file: &str) -> Vec<Entry> {
    let s = text.as_bytes();
    let mut p = Parser { s, i: 0 };
    let mut strings = month_macros();
    let mut out = Vec::new();
    while p.i < s.len() {
        // Outside entries: skip `%` comment lines and junk until the next `@`.
        match s[p.i] {
            b'%' => {
                while p.i < s.len() && s[p.i] != b'\n' {
                    p.i += 1;
                }
                continue;
            }
            b'@' => {}
            _ => {
                p.i += 1;
                continue;
            }
        }
        p.i += 1;
        let kind = p.ident().to_lowercase();
        p.skip_ws();
        let Some(&open) = s.get(p.i) else { break };
        if open != b'{' && open != b'(' {
            continue;
        }
        let close = if open == b'{' { b'}' } else { b')' };
        p.i += 1;
        match kind.as_str() {
            "comment" | "preamble" => {
                // Skip to the matching close.
                let mut depth = 1;
                while p.i < s.len() && depth > 0 {
                    if s[p.i] == open {
                        depth += 1;
                    } else if s[p.i] == close {
                        depth -= 1;
                    }
                    p.i += 1;
                }
            }
            "string" => {
                p.skip_ws();
                let name = p.ident().to_lowercase();
                p.skip_ws();
                if s.get(p.i) == Some(&b'=') {
                    p.i += 1;
                    if let Some(v) = p.value(&strings) {
                        strings.insert(name, v);
                    }
                }
                while p.i < s.len() && s[p.i] != close {
                    p.i += 1;
                }
                p.i += 1;
            }
            _ => {
                p.skip_ws();
                let key = p.ident();
                let mut fields = HashMap::new();
                loop {
                    p.skip_ws();
                    match s.get(p.i) {
                        Some(b',') => {
                            p.i += 1;
                            continue;
                        }
                        Some(c) if *c == close => {
                            p.i += 1;
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                    let name = p.ident().to_lowercase();
                    p.skip_ws();
                    if name.is_empty() || s.get(p.i) != Some(&b'=') {
                        // Not a field: resynchronise at the next comma or the end of the entry.
                        while p.i < s.len() && s[p.i] != b',' && s[p.i] != close {
                            p.i += 1;
                        }
                        continue;
                    }
                    p.i += 1;
                    match p.value(&strings) {
                        Some(v) => {
                            fields.insert(name, v.split_whitespace().collect::<Vec<_>>().join(" "));
                        }
                        None => break,
                    }
                }
                if !key.is_empty() {
                    out.push(Entry {
                        key,
                        kind,
                        fields,
                        file: file.to_string(),
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_world_bibtex() {
        let t = r#"
% A comment with @fake{entry}
@String{acl = "Association for Computational Linguistics"}
@Article{Smith2020,
  title = {The {BERT} Model: {Deep} Learning},
  author = "Smith, John and M{\"u}ller, Hans",
  year = 2020,
  publisher = acl # { Press},
  month = jan,
}
@inproceedings ( doe21 , title={A "quoted" title}, doi = {https://doi.org/10.1/X} )
@comment{ignored @article{nope, title={x}}}
"#;
        let e = parse(t, "refs.bib");
        assert_eq!(e.len(), 2);
        assert_eq!(
            (e[0].key.as_str(), e[0].kind.as_str()),
            ("Smith2020", "article")
        );
        assert_eq!(e[0].field("title"), "The {BERT} Model: {Deep} Learning");
        assert_eq!(e[0].field("author"), "Smith, John and M{\\\"u}ller, Hans");
        assert_eq!(e[0].field("year"), "2020");
        assert_eq!(
            e[0].field("publisher"),
            "Association for Computational Linguistics Press"
        );
        assert_eq!(e[1].key, "doe21");
        assert_eq!(e[1].field("title"), "A \"quoted\" title");
    }
}
