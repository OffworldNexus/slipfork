//! Byte-exact status-frame and append-only emission pins.

use std::sync::{Arc, Mutex};

use super::StatusLine;
use crate::boot::build::LiveSteps;
use crate::boot::progress::BootProgress;
use crate::ui::report::Palette;
use crate::ui::report::Pane;

fn status() -> StatusLine {
    StatusLine::new(
        BootProgress::new("Booting demo"),
        Arc::new(Mutex::new(LiveSteps::new())),
        Pane::new(110),
    )
}

#[test]
fn the_status_frame_is_exactly_carriage_return_erase_and_the_line() {
    let status = status();
    let frame = status.frame_text('⠋');
    assert!(frame.starts_with(Pane::PREFIX), "{frame:?}");
    assert_eq!(
        frame.matches(Pane::PREFIX).count(),
        1,
        "exactly one status frame: {frame:?}"
    );
    assert!(
        !frame.ends_with('\n'),
        "the status line is never newline-terminated"
    );
    assert!(frame.contains("Booting demo"), "{frame:?}");
    assert!(frame.contains("0/0"), "{frame:?}");
}

#[test]
fn the_plain_palette_rows_carry_zero_escapes_and_zero_carriage_returns() {
    let palette = Palette::new(false);
    for row in [
        palette.icon_ok() + " row",
        palette.meta("1ms"),
        palette.ready("✓ Ready"),
        palette.next("slipfork up"),
    ] {
        assert!(!row.contains('\u{1b}') && !row.contains('\r'), "{row:?}");
    }
}
