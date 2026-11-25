//! The one SGR helper. Colour is a boolean resolved once by the composition root.

/// Wraps `text` in `ESC[<code>m` only when the composition root allowed colour.
///
/// `code` is a raw SGR parameter (`1`, `2`, `31`, `1;36`, …) so callers keep their existing
/// palette; the live boot pane layers named styles from `owo-colors` on top of this frame.
#[must_use]
pub(super) fn paint(text: &str, code: &str, color: bool) -> String {
    if color {
        format!("\u{1b}[{code}m{text}\u{1b}[0m")
    } else {
        text.to_owned()
    }
}
