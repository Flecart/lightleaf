//! `{{name}}` placeholders in prompt files.

/// Replaces each `{{key}}` of `template` with its value, in one pass: text inside the values
/// (a paper, a writing guide) is never scanned for placeholders. Unknown placeholders are left
/// as they are, so a missing value shows up in tests instead of silently disappearing.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find("}}").and_then(|j| {
            vars.iter()
                .find(|(k, _)| *k == &after[..j])
                .map(|(_, v)| (j, v))
        }) {
            Some((j, v)) => {
                out.push_str(v);
                rest = &after[j + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Placeholders still present in `s` (for tests: every prompt must be fully rendered).
#[cfg(test)]
pub fn unresolved(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("{{") {
        let after = &rest[i + 2..];
        match after.find("}}") {
            Some(j)
                if after[..j]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && j > 0 =>
            {
                out.push(after[..j].to_string());
                rest = &after[j + 2..];
            }
            _ => rest = after,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_reports_leftovers() {
        assert_eq!(
            render("Hi {{name}}, {{name}}!", &[("name", "Ada")]),
            "Hi Ada, Ada!"
        );
        // Values are not re-scanned, and unknown placeholders stay.
        assert_eq!(render("{{a}} {{b}}", &[("a", "{{b}}")]), "{{b}} {{b}}");
        assert_eq!(unresolved("a {{x}} b {{y_1}} {{ not }} {{"), ["x", "y_1"]);
        // BibTeX-like double braces are not placeholders.
        assert!(unresolved("title={{PaperMentor}: x}").is_empty());
    }
}
