//! The one human failure block: `error:` lines plus exactly one `next:` remedy.
//!
//! Daemon messages arrive already colour-free. This renderer owns the only `error:` / `next:`
//! roles, so `main` and the boot pane cannot drift: a typed `✗` verdict keeps
//! its glyph, and empty lines inside a daemon message are dropped so the block starts immediately
//! after the last live row (at most one blank line in scrollback).

use super::Palette;

/// Splits an embedded `next: <remedy>` line out of a daemon message.
///
/// This lives with the renderer because `next:` is a UI role; every caller shares one splitter.
#[must_use]
pub fn split_remedy(message: &str) -> (String, Option<String>) {
    let mut lines = Vec::new();
    let mut remedy = None;
    for line in message.lines() {
        if let Some(next) = line.strip_prefix("next: ") {
            remedy.get_or_insert_with(|| next.to_owned());
        } else if !line.is_empty() {
            lines.push(line);
        }
    }
    (lines.join("\n"), remedy)
}

/// Renders `message` (which may carry its own trailing `next:`) and `fallback` remedy into the
/// exact lines a human reads. `fallback` is used only when the message carries no remedy.
#[must_use]
pub fn block(message: &str, fallback: &str, palette: Palette) -> Vec<String> {
    let (body, embedded) = split_remedy(message);
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next().unwrap_or("the command failed");
    let mut rows = Vec::new();
    if let Some(rest) = first.strip_prefix("✗ ") {
        rows.push(format!("{} {}", palette.icon_bad(), palette.value(rest)));
        for line in lines {
            rows.push(palette.value(line));
        }
    } else {
        let first = first.strip_prefix("error: ").unwrap_or(first);
        rows.push(format!(
            "{} {}",
            palette.error("error:"),
            palette.error(first)
        ));
        for line in lines {
            rows.push(palette.error(line));
        }
    }
    let next = embedded
        .as_deref()
        .unwrap_or(fallback)
        .lines()
        .next()
        .unwrap_or_default()
        .trim();
    if !next.is_empty() {
        rows.push(format!(
            "{} {}",
            palette.meta("next:"),
            palette.tint(next, "36")
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::block;
    use crate::ui::Palette;

    #[test]
    fn no_color_prints_one_error_line_and_one_next_without_escapes() {
        let rows = block(
            "build `api` failed at `RUN apt-get update`: expired metadata\n\nnext: slipfork logs -r 3f9c21",
            "retry",
            Palette::new(false),
        );
        assert_eq!(
            rows,
            vec![
                "error: build `api` failed at `RUN apt-get update`: expired metadata".to_owned(),
                "next: slipfork logs -r 3f9c21".to_owned(),
            ]
        );
        assert!(rows.iter().all(|row| !row.contains('\u{1b}')));
    }

    #[test]
    fn colored_roles_are_red_error_meta_next_and_cyan_command() {
        let rows = block(
            "cannot bind 127.0.0.1:80",
            "use another port",
            Palette::new(true),
        );
        assert_eq!(
            rows,
            vec![
                "\u{1b}[31merror:\u{1b}[0m \u{1b}[31mcannot bind 127.0.0.1:80\u{1b}[0m".to_owned(),
                "\u{1b}[38;5;244mnext:\u{1b}[0m \u{1b}[36muse another port\u{1b}[0m".to_owned(),
            ]
        );
    }

    #[test]
    fn a_typed_verdict_keeps_its_glyph_and_still_gets_one_next() {
        assert_eq!(
            block(
                "✗ RUN api failed (exit 3)",
                "slipfork logs -r abc123",
                Palette::new(false)
            ),
            vec![
                "✗ RUN api failed (exit 3)".to_owned(),
                "next: slipfork logs -r abc123".to_owned(),
            ]
        );
    }
}
