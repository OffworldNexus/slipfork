//! Minimal line diff for `format_check`.

/// Returns an empty string when `original` already equals `formatted`.
pub(crate) fn line_diff(original: &str, formatted: &str, label: &str) -> String {
    if original == formatted {
        return String::new();
    }
    let before: Vec<&str> = original.lines().collect();
    let after: Vec<&str> = formatted.lines().collect();
    let mut prefix = 0;
    while prefix < before.len() && prefix < after.len() && before[prefix] == after[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < before.len().saturating_sub(prefix)
        && suffix < after.len().saturating_sub(prefix)
        && before[before.len() - 1 - suffix] == after[after.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let mut out = format!("--- {label}\n+++ {label} (formatted)\n");
    if prefix == before.len() && prefix == after.len() {
        // Only the presence of a final newline differs: still emit a real hunk.
        out.push_str(&format!("@@ line {} @@\n", prefix.max(1)));
        let last = before.last().copied().unwrap_or("");
        out.push_str(&format!("-{last}\n+{last}\n"));
        return out;
    }
    out.push_str(&format!("@@ line {} @@\n", prefix + 1));
    for line in &before[prefix..before.len() - suffix] {
        out.push_str(&format!("-{line}\n"));
    }
    for line in &after[prefix..after.len() - suffix] {
        out.push_str(&format!("+{line}\n"));
    }
    out
}
