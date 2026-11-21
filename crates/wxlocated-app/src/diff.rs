//! Git-style patch builder and line change counter for comparing an Agent
//! semantic layer with static reconstruction.

use similar::{ChangeTag, TextDiff};

/// Builds a Git-style unified patch (`--- a/...` / `+++ b/...`) for `DiffFile::parse`.
/// Returns `None` when `old == new`.
pub fn git_patch(path: &str, old: &str, new: &str) -> Option<String> {
    if old == new {
        return None;
    }
    let normalized_old = ensure_trailing_newline(old);
    let normalized_new = ensure_trailing_newline(new);
    let diff = TextDiff::from_lines(normalized_old.as_ref(), normalized_new.as_ref());
    let patch = diff
        .unified_diff()
        .context_radius(4)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string();
    (!patch.trim().is_empty()).then_some(patch)
}

fn ensure_trailing_newline(text: &str) -> std::borrow::Cow<'_, str> {
    if text.is_empty() || text.ends_with('\n') {
        std::borrow::Cow::Borrowed(text)
    } else {
        let mut owned = String::with_capacity(text.len() + 1);
        owned.push_str(text);
        owned.push('\n');
        std::borrow::Cow::Owned(owned)
    }
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
    use super::{counts, git_patch};

    #[test]
    fn counts_changed_lines() {
        assert_eq!(counts("a\nb\nc\n", "a\nB\nc\nd\n"), (1, 2));
        assert_eq!(counts("same\n", "same\n"), (0, 0));
    }

    #[test]
    fn git_patch_parses_with_component_diff_and_handles_no_trailing_newline() {
        assert!(git_patch("a.js", "same", "same").is_none());
        let patch = git_patch("pages/index.js", "const a = 1;", "const a = 2;\nconst b = 3;").expect("patch");
        let parsed = gpui_kit::component::diff::DiffFile::parse(&patch).expect("valid diff patch");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].path().as_ref(), "pages/index.js");
    }
}
