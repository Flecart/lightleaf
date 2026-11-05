//! Text normalisation and similarity for matching references (as upstream `CitationVerifier`,
//! plus LaTeX accent decoding and whole-word venue acronyms).

/// Folds an accented Latin letter to its base letter (é → e, ß → ss, ø → o, ...).
fn fold(c: char, out: &mut String) {
    let base = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' => "A",
        'ç' | 'ć' | 'č' | 'ĉ' | 'ċ' => "c",
        'Ç' | 'Ć' | 'Č' | 'Ĉ' | 'Ċ' => "C",
        'ď' | 'đ' => "d",
        'Ď' | 'Đ' => "D",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ė' | 'Ę' | 'Ě' => "E",
        'ğ' | 'ģ' => "g",
        'Ğ' | 'Ģ' => "G",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' | 'ı' => "i",
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ī' | 'Į' | 'İ' => "I",
        'ł' | 'ľ' | 'ĺ' | 'ļ' => "l",
        'Ł' | 'Ľ' | 'Ĺ' | 'Ļ' => "L",
        'ñ' | 'ń' | 'ň' | 'ņ' => "n",
        'Ñ' | 'Ń' | 'Ň' | 'Ņ' => "N",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ő' => "O",
        'ř' | 'ŕ' => "r",
        'Ř' | 'Ŕ' => "R",
        'ś' | 'š' | 'ş' | 'ș' => "s",
        'Ś' | 'Š' | 'Ş' | 'Ș' => "S",
        'ß' => "ss",
        'ť' | 'ţ' | 'ț' => "t",
        'Ť' | 'Ţ' | 'Ț' => "T",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => "u",
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ū' | 'Ů' | 'Ű' | 'Ų' => "U",
        'ý' | 'ÿ' => "y",
        'Ý' | 'Ÿ' => "Y",
        'ž' | 'ź' | 'ż' => "z",
        'Ž' | 'Ź' | 'Ż' => "Z",
        'æ' => "ae",
        'Æ' => "AE",
        'œ' => "oe",
        'Œ' => "OE",
        c if ('\u{300}'..='\u{36f}').contains(&c) => "",
        c => {
            out.push(c);
            return;
        }
    };
    out.push_str(base);
}

pub fn fold_accents(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        fold(c, &mut out);
    }
    out
}

/// Decodes LaTeX accent and letter macros to plain letters: `{\"u}`, `\"{u}`, `\'e`, `\v{s}`,
/// `\ss`, `\o`, `\aa` ... (upstream left these in and mangled surnames like `M{\"u}ller`).
fn decode_accents(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == '\\' && i + 1 < b.len() {
            let c = b[i + 1];
            // Symbol accents: \" \' \` \^ \~ \= \.
            if matches!(c, '"' | '\'' | '`' | '^' | '~' | '=' | '.')
                || (matches!(c, 'v' | 'u' | 'H' | 'c' | 'k' | 'r')
                    && i + 2 < b.len()
                    && (b[i + 2] == '{' || b[i + 2] == ' '))
            {
                let mut j = i + 2;
                while j < b.len() && (b[j] == '{' || b[j] == ' ') {
                    j += 1;
                }
                if j < b.len() && b[j].is_alphabetic() {
                    // `{\"u}`: drop the group's opening brace too (its `}` is skipped below).
                    if out.ends_with('{') {
                        out.pop();
                    }
                    out.push(b[j]);
                    j += 1;
                    while j < b.len() && b[j] == '}' {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
            }
            // Letter macros.
            let word: String = b[i + 1..]
                .iter()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            let letter = match word.as_str() {
                "ss" => Some("ss"),
                "o" => Some("o"),
                "O" => Some("O"),
                "aa" => Some("a"),
                "AA" => Some("A"),
                "ae" => Some("ae"),
                "AE" => Some("AE"),
                "oe" => Some("oe"),
                "OE" => Some("OE"),
                "l" => Some("l"),
                "L" => Some("L"),
                "i" => Some("i"),
                _ => None,
            };
            if let Some(l) = letter {
                out.push_str(l);
                i += 1 + word.len();
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Upstream `stripLatex`: commands (with an opening brace) and braces become spaces.
pub fn strip_latex(s: &str) -> String {
    let s = decode_accents(s);
    let b: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == '\\' && i + 1 < b.len() && b[i + 1].is_ascii_alphabetic() {
            i += 1;
            while i < b.len() && b[i].is_ascii_alphabetic() {
                i += 1;
            }
            while i < b.len() && b[i].is_whitespace() {
                i += 1;
            }
            if i < b.len() && b[i] == '{' {
                i += 1;
            }
            out.push(' ');
            continue;
        }
        out.push(if b[i] == '{' || b[i] == '}' {
            ' '
        } else {
            b[i]
        });
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

const PUNCT: &[char] = &[
    '.', ',', ';', ':', '!', '?', '\'', '"', '`', '‘', '’', '“', '”', '(', ')', '[', ']', '-',
];

pub fn normalize_title(s: &str) -> String {
    let t: String = fold_accents(&strip_latex(s))
        .to_lowercase()
        .chars()
        .map(|c| if PUNCT.contains(&c) { ' ' } else { c })
        .collect();
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn normalize_author(s: &str) -> String {
    let t: String = fold_accents(s)
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_whitespace() || *c == '-')
        .collect();
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Surnames from a BibTeX `author` field ("Last, First" or "First Last", joined by `and`).
pub fn surnames(author_field: &str) -> Vec<String> {
    let s = strip_latex(author_field);
    split_and(&s)
        .into_iter()
        .map(|a| {
            let a = a.trim();
            match a.split_once(',') {
                Some((last, _)) => normalize_author(last),
                None => normalize_author(a.split_whitespace().last().unwrap_or("")),
            }
        })
        .filter(|n| !n.is_empty())
        .collect()
}

fn split_and(s: &str) -> Vec<&str> {
    let lower = s.to_lowercase();
    let mut out = Vec::new();
    let mut last = 0;
    let mut from = 0;
    while let Some(i) = lower[from..].find(" and ") {
        let at = from + i;
        out.push(&s[last..at]);
        last = at + 5;
        from = last;
    }
    out.push(&s[last..]);
    out
}

pub fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 1 − edit distance / longer length, on normalised titles (0 when either is empty).
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let (na, nb): (Vec<char>, Vec<char>) = (
        normalize_title(a).chars().collect(),
        normalize_title(b).chars().collect(),
    );
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }
    1.0 - levenshtein(&na, &nb) as f64 / na.len().max(nb.len()) as f64
}

fn normalize_venue(s: &str) -> String {
    let mut t = strip_latex(s).to_lowercase();
    for p in ["proceedings of the ", "proceedings of "] {
        t = t.replace(p, "");
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    let mut kept = Vec::new();
    let mut k = 0;
    while k < words.len() {
        let w = words[k];
        // "the 12th annual international conference on" and similar boilerplate.
        let ordinal = w.len() > 2
            && w[..w.len() - 2].chars().all(|c| c.is_ascii_digit())
            && ["st", "nd", "rd", "th"].contains(&&w[w.len() - 2..]);
        let year = w.len() == 4
            && (w.starts_with("19") || w.starts_with("20"))
            && w.chars().all(|c| c.is_ascii_digit());
        if matches!(w, "the" | "annual" | "international")
            || ordinal
            || year
            || (w == "conference" && words.get(k + 1) == Some(&"on"))
        {
            k += if w == "conference" { 2 } else { 1 };
            continue;
        }
        kept.push(w);
        k += 1;
    }
    let t: String = kept
        .join(" ")
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == ' ' {
                c
            } else {
                ' '
            }
        })
        .collect();
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

const VENUE_ALIASES: &[&[&str]] = &[
    &[
        "international conference on learning representations",
        "iclr",
    ],
    &[
        "neural information processing systems",
        "nips",
        "neurips",
        "advances in neural",
    ],
    &["international conference on machine learning", "icml"],
    &[
        "association for computational linguistics",
        "acl",
        "annual meeting",
    ],
    &["empirical methods in natural language processing", "emnlp"],
    &["north american chapter", "naacl"],
    &["aaai conference on artificial intelligence", "aaai"],
    &["computer vision and pattern recognition", "cvpr"],
    &["international conference on computer vision", "iccv"],
    &["european conference on computer vision", "eccv"],
    &["conference on language modeling", "colm"],
    &["journal of machine learning research", "jmlr"],
    &["arxiv", "corr", "preprint"],
];

/// `phrase` occurs in `text` as whole words (upstream used plain substrings, so "acl"
/// matched "miracle").
fn has_words(text: &str, phrase: &str) -> bool {
    let t: Vec<&str> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let p: Vec<&str> = phrase.split_whitespace().collect();
    !p.is_empty() && t.windows(p.len()).any(|w| w == p.as_slice())
}

/// Whether two venue strings name the same venue: `Some(true/false)`, or `None` when unknown.
pub fn venues_agree(bib: &str, s2: &str) -> Option<bool> {
    let (a_raw, b_raw) = (
        strip_latex(bib).to_lowercase(),
        strip_latex(s2).to_lowercase(),
    );
    if a_raw.is_empty() || b_raw.is_empty() {
        return None;
    }
    if VENUE_ALIASES.iter().any(|al| {
        al.iter().any(|x| has_words(&a_raw, x)) && al.iter().any(|x| has_words(&b_raw, x))
    }) {
        return Some(true);
    }
    let (a, b) = (normalize_venue(bib), normalize_venue(s2));
    if a.is_empty() || b.is_empty() {
        return None;
    }
    if a == b || has_words(&a, &b) || has_words(&b, &a) {
        return Some(true);
    }
    let ta: std::collections::HashSet<&str> = a.split(' ').filter(|w| w.len() > 2).collect();
    let tb: std::collections::HashSet<&str> = b.split(' ').filter(|w| w.len() > 2).collect();
    if ta.is_empty() || tb.is_empty() {
        return None;
    }
    Some(ta.intersection(&tb).count() as f64 / ta.len().min(tb.len()) as f64 >= 0.5)
}

/// A DOI without URL or `doi:` prefixes, lowercased (upstream compared them raw).
pub fn normalize_doi(s: &str) -> String {
    let t = s.trim().to_lowercase();
    let t = t
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let t = t
        .trim_start_matches("dx.doi.org/")
        .trim_start_matches("doi.org/")
        .trim_start_matches("doi:");
    t.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_accents() {
        assert_eq!(
            normalize_title("The {BERT} Model: Pre-training \\textbf{Deep} Transformers"),
            "the bert model pre training deep transformers"
        );
        assert_eq!(strip_latex("G\\\"{o}del"), "Godel");
        assert!(title_similarity("Attention Is All You Need", "Attention is all you need.") > 0.99);
        assert!(title_similarity("Attention Is All You Need", "A totally different paper") < 0.5);
        assert_eq!(title_similarity("", "x"), 0.0);
    }

    #[test]
    fn author_surnames() {
        assert_eq!(
            surnames("M{\\\"u}ller, Hans and Jane Smith and others"),
            ["muller", "smith", "others"]
        );
        assert_eq!(
            surnames("von Neumann, J. AND Kurt G\\\"{o}del"),
            ["von neumann", "godel"]
        );
        assert!(surnames("").is_empty());
    }

    #[test]
    fn venues() {
        assert_eq!(
            venues_agree(
                "Advances in Neural Information Processing Systems",
                "NeurIPS"
            ),
            Some(true)
        );
        assert_eq!(
            venues_agree(
                "Proceedings of the 2023 Conference on Empirical Methods in Natural Language Processing",
                "EMNLP"
            ),
            Some(true)
        );
        assert_eq!(venues_agree("Miracle Workshop", "ACL"), Some(false));
        assert_eq!(venues_agree("", "ACL"), None);
        assert_eq!(
            normalize_doi(" https://doi.org/10.1000/ABC "),
            "10.1000/abc"
        );
        assert_eq!(normalize_doi("doi:10.1/x"), "10.1/x");
    }
}
