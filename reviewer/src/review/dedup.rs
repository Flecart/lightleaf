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

/// Two comments say the same thing: in the same file, their passages overlap by at least half
/// of the shorter one, and their texts are similar (Dice ≥ 0.4 after lowercasing).
fn duplicates(a: &Comment, b: &Comment) -> bool {
    if a.file != b.file {
        return false;
    }
    let overlap = a
        .range
        .1
        .min(b.range.1)
        .saturating_sub(a.range.0.max(b.range.0));
    if overlap == 0 {
        return false;
    }
    let shorter = (a.range.1 - a.range.0).min(b.range.1 - b.range.0).max(1);
    overlap * 2 >= shorter && dice(&a.text, &b.text) >= 0.4
}

/// Which of two duplicates to drop: the less severe; on a tie, the one from a whole-paper
/// reviewer when only one is; otherwise the shorter comment (the later one on equal length).
fn loser(a: &Comment, b: &Comment) -> Side {
    if a.severity != b.severity {
        return if a.severity < b.severity {
            Side::First
        } else {
            Side::Second
        };
    }
    if a.broad != b.broad {
        return if a.broad { Side::First } else { Side::Second };
    }
    if a.text.chars().count() < b.text.chars().count() {
        Side::First
    } else {
        Side::Second
    }
}

enum Side {
    First,
    Second,
}

/// Removes duplicates pairwise in input order. Returns (kept, number removed).
pub fn dedup(comments: Vec<Comment>) -> (Vec<Comment>, usize) {
    let n = comments.len();
    let mut removed = vec![false; n];
    for i in 0..n {
        for j in i + 1..n {
            if removed[i] {
                break;
            }
            if removed[j] || !duplicates(&comments[i], &comments[j]) {
                continue;
            }
            match loser(&comments[i], &comments[j]) {
                Side::First => removed[i] = true,
                Side::Second => removed[j] = true,
            }
        }
    }
    let merged = removed.iter().filter(|r| **r).count();
    let kept = comments
        .into_iter()
        .zip(removed)
        .filter(|(_, r)| !r)
        .map(|(c, _)| c)
        .collect();
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
            broad: false,
        }
    }

    #[test]
    fn broad_reviewer_loses_ties() {
        let mut a = c(
            Severity::Warning,
            "Add a citation for this claim here.",
            (0, 50),
        );
        a.broad = true;
        let b = c(
            Severity::Warning,
            "Add a citation for this claim.",
            (10, 50),
        );
        let (kept, _) = dedup(vec![a, b]);
        assert_eq!(kept.len(), 1);
        assert!(!kept[0].broad);
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
