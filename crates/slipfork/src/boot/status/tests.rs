//! Byte-exact status-frame and append-only emission pins.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::StatusLine;
use crate::boot::build::LiveSteps;
use crate::boot::progress::BootProgress;
use crate::boot::render::status_line;
use crate::ui::Palette;
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
fn the_plain_fallback_line_carries_zero_escapes_and_zero_carriage_returns() {
    let line = status_line(
        '⠋',
        crate::boot::render::BarProgress::outer(1, 3),
        "Booting demo",
        Duration::from_millis(10),
        110,
    );
    assert!(!line.contains('\u{1b}'), "{line:?}");
    assert!(!line.contains('\r'), "{line:?}");
    let rows = [
        Palette::new(false).icon_ok() + " row",
        Palette::new(false).meta("1ms"),
    ];
    for row in rows {
        assert!(!row.contains('\u{1b}') && !row.contains('\r'), "{row:?}");
    }
}
