//! ANSI styling for the shipped CLI. One helper, one policy.

/// Wraps `text` in `ESC[<code>m` only when the composition root allowed colour.
#[must_use]
pub fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("\u{1b}[{code}m{text}\u{1b}[0m")
    } else {
        text.to_owned()
    }
}
