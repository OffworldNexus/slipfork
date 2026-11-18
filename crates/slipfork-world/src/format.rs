//! Shared diagnostic rendering for declared ids and stage names.
//!
//! `slipfork-dag` (`[world]` selection) and `slipfork-engine` (`Dockerfile` contract) both name
//! the exact candidates a user can pick from in their `fix:` line; this is the one renderer.

/// Render `items` as `'api', 'worker'` for diagnostics; `none` when the list is empty.
pub fn quoted(items: &[String]) -> String {
    if items.is_empty() {
        return "none".to_owned();
    }
    items
        .iter()
        .map(|item| format!("'{item}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::quoted;

    #[test]
    fn quoted_renders_candidates_or_none() {
        assert_eq!(quoted(&[]), "none");
        assert_eq!(
            quoted(&["api".to_owned(), "worker".to_owned()]),
            "'api', 'worker'"
        );
    }
}
