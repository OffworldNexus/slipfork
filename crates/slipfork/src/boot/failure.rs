//! The one boot-failure envelope: verdict, `-v` runtime plumbing, and exactly one `next:`.
//!
//! A failure is never prose. The typed `failed` frame already carries the phase, the target, the
//! exit code, the replay command, and (for `-v` only) the container plumbing, so this module only
//! turns those fields into the three lines the contract allows — no `Failed to …` sentence, no
//! embedded-`next:` parsing, and no `crun` text unless the caller asked for `-v`.

use serde_json::Value;

use crate::ui::{Palette, Verdict};

/// The verdict and its remedy: `✗ RUN api failed (exit 3)`, an optional dim `runtime:` row, and
/// one `  next: <cyan command>`.
///
/// `verb` is the same token the phase row used (`RUN`, `start`, `hook`), so the two lines read as
/// one story. Rows are plain when the palette has colour off, and the envelope never prints empty
/// fields.
pub(super) fn envelope(event: &Value, verb: &str, verbose: bool, palette: Palette) -> Vec<String> {
    let target = event["target"]
        .as_str()
        .or_else(|| event["name"].as_str())
        .unwrap_or("step");
    let exit_code = event["exit_code"]
        .as_i64()
        .and_then(|code| i32::try_from(code).ok());
    let mut rows = vec![palette.verdict(&Verdict {
        verb: verb.to_owned(),
        target: target.to_owned(),
        state: "failed".to_owned(),
        exit_code,
        duration_ms: None,
    })];
    if verbose && let Some(runtime) = event["runtime"].as_str().filter(|text| !text.is_empty()) {
        rows.push(format!("  {}", palette.dim(&format!("runtime: {runtime}"))));
    }
    if let Some(next) = event["next"].as_str().filter(|text| !text.is_empty()) {
        rows.push(format!(
            "  {} {}",
            palette.dim("next:"),
            palette.tint(next, "36")
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::envelope;
    use crate::ui::Palette;

    fn failed() -> serde_json::Value {
        json!({
            "event": "failed",
            "phase": "run",
            "target": "api",
            "exit_code": 3,
            "run_id": "5be421",
            "next": "slipfork logs -r 5be421",
            "runtime": "crun exited with code 3",
        })
    }

    #[test]
    fn one_verdict_and_one_next_with_no_runtime_unless_verbose() {
        let rows = envelope(&failed(), "RUN", false, Palette::new(false));
        assert_eq!(
            rows,
            vec![
                "✗ RUN api failed (exit 3)".to_owned(),
                "  next: slipfork logs -r 5be421".to_owned(),
            ]
        );
        let block = rows.join("\n");
        assert_eq!(block.matches("next:").count(), 1, "{block}");
        assert!(!block.contains("crun"), "{block}");
        assert!(!block.contains("Failed to"), "{block}");
        assert!(!block.contains('\u{1b}'), "{block}");
    }

    #[test]
    fn verbose_adds_the_dim_runtime_row_before_the_remedy() {
        let rows = envelope(&failed(), "RUN", true, Palette::new(false));
        assert_eq!(
            rows,
            vec![
                "✗ RUN api failed (exit 3)".to_owned(),
                "  runtime: crun exited with code 3".to_owned(),
                "  next: slipfork logs -r 5be421".to_owned(),
            ]
        );
    }

    #[test]
    fn the_remedy_is_actionable_cyan_and_the_verdict_is_red() {
        let rows = envelope(&failed(), "RUN", false, Palette::new(true));
        assert!(rows[0].contains("\u{1b}[31m✗"), "{rows:?}");
        assert!(rows[0].contains("\u{1b}[31mfailed"), "{rows:?}");
        assert!(
            rows[1].contains("\u{1b}[36mslipfork logs -r 5be421"),
            "{rows:?}"
        );
        assert!(
            rows.iter().all(|row| !row.contains("\u{1b}[9")),
            "no bright codes: {rows:?}"
        );
    }

    #[test]
    fn a_missing_exit_code_and_remedy_never_print_empty_fields() {
        let event = json!({ "event": "failed", "target": "web", "next": "" });
        let rows = envelope(&event, "start", false, Palette::new(false));
        assert_eq!(rows, vec!["✗ start web failed".to_owned()]);
    }
}
