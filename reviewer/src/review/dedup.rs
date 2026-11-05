//! Merging duplicate comments from different reviewers.

use super::Comment;
use std::collections::HashMap;

/// Bigram Dice similarity of two strings (0..=1), ignoring case and whitespace.
pub fn dice(a: &str, b: &str) -> f64 {
    let grams = |s: &str| {
        let chars: Vec<char> = s
            .to_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let mut m: HashMap<(char, char), usize> = HashMap::new();
        for w in chars.windows(2) {
            *m.entry((w[0], w[1])).or_default() += 1;
        }
        m
    };
    let (ga, gb) = (grams(a), grams(b));
    let (na, nb): (usize, usize) = (ga.values().sum(), gb.values().sum());
    if na + nb == 0 {
        return 0.0;
    }
    let common: usize = ga
        .iter()
        .map(|(k, v)| (*v).min(*gb.get(k).unwrap_or(&0)))
        .sum();
    2.0 * common as f64 / (na + nb) as f64
}

/// Two comments say the same thing: overlapping passages (≥ 50 % of the shorter one) with
/// similar text (Dice ≥ 0.4), or near-identical text anywhere in the same file (Dice ≥ 0.8).
fn duplicates(a: &Comment, b: &Comment) -> bool {
    if a.file != b.file {
        return false;
    }
    let overlap = a
        .range
        .1
        .min(b.range.1)
        .saturating_sub(a.range.0.max(b.range.0));
    let shorter = (a.range.1 - a.range.0).min(b.range.1 - b.range.0).max(1);
    let sim = dice(&a.text, &b.text);
    (overlap * 2 >= shorter && sim >= 0.4) || sim >= 0.8
}

/// Keeps one comment per group of duplicates (the most severe). Returns (kept, merged count).
pub fn dedup(mut comments: Vec<Comment>) -> (Vec<Comment>, usize) {
    comments.sort_by(|a, b| b.severity.cmp(&a.severity));
    let mut kept: Vec<Comment> = Vec::new();
    let mut merged = 0;
    for c in comments {
        if kept.iter().any(|k| duplicates(k, &c)) {
            merged += 1;
        } else {
            kept.push(c);
        }
    }
    (kept, merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::Severity;

    fn c(sev: Severity, text: &str, range: (usize, usize)) -> Comment {
        Comment {
            agent: "A".into(),
            severity: sev,
            text: text.into(),
            quote: String::new(),
            file: "main.tex".into(),
            range,
        }
    }

    #[test]
    fn merges_similar_overlapping_comments_keeping_the_most_severe() {
        let (kept, merged) = dedup(vec![
            c(
                Severity::Suggestion,
                "This claim needs a citation to prior work.",
                (10, 60),
            ),
            c(
                Severity::Warning,
                "This claim needs a citation of prior work!",
                (20, 70),
            ),
            c(
                Severity::Suggestion,
                "Define the acronym before using it.",
                (15, 40),
            ),
        ]);
        assert_eq!(merged, 1);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().any(|k| k.severity == Severity::Warning));
    }

    #[test]
    fn dice_similarity() {
        assert!(dice("needs a citation", "needs a citation") > 0.99);
        assert!(dice("needs a citation", "define the acronym") < 0.3);
    }
}
