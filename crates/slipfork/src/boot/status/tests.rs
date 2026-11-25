//! Byte-exact status-line and append-only emission pins.

use std::sync::{Arc, Mutex};

use super::StatusLine;
use crate::boot::build::LiveSteps;
use crate::boot::progress::BootProgress;
use crate::boot::render::status_line;
use crate::ui::Palette;
use std::time::Duration;

/// Every escape a boot run may never emit.
const FORBIDDEN: [&str; 9] = [
    "\x1b[J",
    "\x1b[r",
    "\x1bc",
    "\x1b[?1049",
    "\x1b[?47",
    "\x1b[?1047",
    "\x1b[6n",
    "\x1b[1;1H",
    "\x1b[0;1H",
];

fn status() -> StatusLine {
    StatusLine::new(
        BootProgress::new("Booting demo"),
        110,
        Arc::new(Mutex::new(LiveSteps::new())),
    )
}

fn assert_no_forbidden(bytes: &str) {
    for forbidden in FORBIDDEN {
        assert!(
            !bytes.contains(forbidden),
            "forbidden {forbidden:?}: {bytes:?}"
        );
    }
    // Any CSI whose final byte moves/addresses the cursor, sets margins, or erases.
    let raw = bytes.as_bytes();
    let mut index = 0;
    while index + 1 < raw.len() {
        if raw[index] == 0x1b && raw[index + 1] == b'[' {
            let mut at = index + 2;
            while at < raw.len() && (raw[at].is_ascii_digit() || raw[at] == b';' || raw[at] == b'?')
            {
                at += 1;
            }
            if let Some(final_byte) = raw.get(at) {
                assert!(
                    !matches!(final_byte, b'H' | b'f' | b'r' | b'J'),
                    "cursor/scroll/erase CSI in {bytes:?}"
                );
            }
        }
        index += 1;
    }
}

#[test]
fn the_status_frame_is_exactly_carriage_return_erase_and_the_line() {
    let status = status();
    let frame = status.frame_text('⠋');
    assert!(frame.starts_with("\r\x1b[2K"), "{frame:?}");
    assert_eq!(&frame[..5], "\r\x1b[2K");
    assert!(
        !frame.ends_with('\n'),
        "the status line is never newline-terminated"
    );
    assert!(frame.contains("Booting demo"), "{frame:?}");
    assert!(frame.contains("0/0"), "{frame:?}");
    assert_no_forbidden(&frame);
}

#[test]
fn emit_clears_the_status_line_once_then_prints_ordinary_rows() {
    let mut status = status();
    let mut out = Vec::new();
    status.emit_to(&["✓ [1/3] image  api  cached  8ms".to_owned()], &mut out);
    let text = String::from_utf8(out.clone()).expect("UTF-8");
    assert!(
        text.starts_with("✓ [1/3] image  api  cached  8ms\n"),
        "{text:?}"
    );
    assert!(
        text.ends_with('m') || text.contains("\r\x1b[2K"),
        "the emit repaints a status frame: {text:?}"
    );
    assert_eq!(
        text.matches("\r\x1b[2K").count(),
        1,
        "one status frame is painted after the row: {text:?}"
    );
    assert_no_forbidden(&text);

    let mut next = Vec::new();
    status.emit_to(&["✓ [2/3] setup  uv sync  done  1ms".to_owned()], &mut next);
    let text = String::from_utf8(next).expect("UTF-8");
    assert!(
        text.starts_with("\r\x1b[2K✓ [2/3] setup  uv sync  done  1ms\n\r\x1b[2K"),
        "a painted status line is erased once before the row and repainted after: {text:?}"
    );
    assert_no_forbidden(&text);
}

#[test]
fn every_carriage_return_is_immediately_followed_by_erase_line() {
    let mut status = status();
    let mut out = Vec::new();
    for row in ["✓ one", "✓ two", "✓ three"] {
        status.emit_to(&[row.to_owned()], &mut out);
    }
    let text = String::from_utf8(out).expect("UTF-8");
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\r' {
            assert_eq!(
                &text[index..index + 5],
                "\r\x1b[2K",
                "a carriage return must be the status frame prefix: {text:?}"
            );
        }
    }
    assert_no_forbidden(&text);
}

#[test]
fn erase_to_is_idempotent_and_writes_the_prefix_at_most_once() {
    let mut status = status();
    let mut out = Vec::new();
    status.emit_to(&["✓ row".to_owned()], &mut out);
    let mut erased = Vec::new();
    status.erase_to(&mut erased);
    assert_eq!(erased, b"\r\x1b[2K", "one erase prefix through the sink");
    assert!(!status.painted, "erase clears the painted flag");
    let mut again = Vec::new();
    status.erase_to(&mut again);
    assert!(again.is_empty(), "a second erase writes nothing: {again:?}");
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
