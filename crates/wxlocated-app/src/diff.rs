//! Unified line diff, for comparing an Agent layer with the static rebuild.

use similar::{ChangeTag, TextDiff};

pub fn unified(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = diff.unified_diff().context_radius(4).header(old_label, new_label).to_string();
    if output.is_empty() {
        output = format!("--- {old_label}\n+++ {new_label}\n（两者内容相同）\n");
    }
    output
}

/// Lines removed and added.
pub fn counts(old: &str, new: &str) -> (usize, usize) {
    let diff = TextDiff::from_lines(old, new);
    let (mut removed, mut added) = (0, 0);
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Delete => removed += 1,
            ChangeTag::Insert => added += 1,
            ChangeTag::Equal => {}
        }
    }
    (removed, added)
}

#[cfg(test)]
mod tests {
    use super::{counts, unified};

    #[test]
    fn counts_changed_lines() {
        assert_eq!(counts("a\nb\nc\n", "a\nB\nc\nd\n"), (1, 2));
        assert_eq!(counts("same\n", "same\n"), (0, 0));
    }

    #[test]
    fn unified_has_headers_or_says_identical() {
        let diff = unified("a\n", "b\n", "静态重建", "Agent");
        assert!(diff.contains("--- 静态重建") && diff.contains("+++ Agent") && diff.contains("-a") && diff.contains("+b"));
        assert!(unified("x\n", "x\n", "A", "B").contains("两者内容相同"));
    }
}
