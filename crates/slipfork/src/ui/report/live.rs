//! The live pane: the one and only writer of `\r\x1b[2K` in the whole workspace.
//!
//! Completed rows are ordinary newline-terminated scrollback lines; only the single boundary
//! line is ever rewritten in place. [`Pane`] owns that byte mechanic and nothing else — no
//! `Palette`, no progress model, no clock, and no ambient terminal probe. A caller renders its
//! content and hands the finished line over.
//!
//! The row budget is fixed by `specs/02-cli-ux.md` (§9.4): the live block repaints at most the
//! active `Phase` headers + `LIVE_LINES` (8) tail lines + one panel line — `O(active + 8)` rows
//! — and never addresses more of the screen than that. Today the pane stays append-only: no
//! `DECSTBM`, no absolute cursor addressing, and no screen clears; every in-place byte is
//! [`Pane::PREFIX`].

use std::io::Write;

/// Braille spinner frames, advanced once per painted status frame.
const BRAILLE: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// The in-place mechanics of one live pane: width, spinner phase, and paint state.
pub struct Pane {
    cols: usize,
    frame: usize,
    painted: bool,
}

impl Pane {
    /// The one in-place-status prefix: carriage return plus erase-line.
    pub const PREFIX: &str = "\r\x1b[2K";

    /// A pane that paints into `cols` columns.
    #[must_use]
    pub const fn new(cols: usize) -> Self {
        Self {
            cols,
            frame: 0,
            painted: false,
        }
    }

    /// The current pane width in columns.
    #[must_use]
    pub const fn cols(&self) -> usize {
        self.cols
    }

    /// Re-measures the pane after a resize; the caller rejects zero widths.
    pub fn resize(&mut self, cols: usize) {
        self.cols = cols;
    }

    /// Advances and returns the next spinner frame.
    #[must_use]
    pub fn spinner(&mut self) -> char {
        let spinner = BRAILLE
            .chars()
            .nth(self.frame % BRAILLE.chars().count())
            .unwrap_or('⠋');
        self.frame = self.frame.wrapping_add(1);
        spinner
    }

    /// One full frame: the prefix plus `line`.
    #[must_use]
    pub fn frame(line: &str) -> String {
        format!("{}{line}", Self::PREFIX)
    }

    /// Writes one frame without flushing; the caller owns the lock and the flush.
    pub fn paint_to(&mut self, line: &str, out: &mut dyn Write) {
        let _ = out.write_all(Self::frame(line).as_bytes());
        self.painted = true;
    }

    /// Emits completed rows as ordinary lines, then repaints the pane with `line`.
    ///
    /// While a frame is painted the cursor sits on the pane line, so one prefix clears it before
    /// the first row; the rows then advance the cursor normally and the final repaint owns the
    /// new bottom line.
    pub fn emit_to(&mut self, rows: &[String], line: &str, out: &mut dyn Write) {
        if rows.is_empty() {
            return;
        }
        if self.painted {
            let _ = out.write_all(Self::PREFIX.as_bytes());
        }
        for row in rows {
            let _ = writeln!(out, "{row}");
        }
        self.painted = false;
        self.paint_to(line, out);
    }

    /// Erases the pane exactly once into `out`; idempotent when called twice.
    pub fn erase_to(&mut self, out: &mut dyn Write) {
        if !self.painted {
            return;
        }
        let _ = out.write_all(Self::PREFIX.as_bytes());
        self.painted = false;
    }
}

#[cfg(test)]
mod tests {
    use super::Pane;

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
                while at < raw.len()
                    && (raw[at].is_ascii_digit() || raw[at] == b';' || raw[at] == b'?')
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
    fn emit_clears_the_status_line_once_then_prints_ordinary_rows() {
        let mut pane = Pane::new(110);
        let mut out = Vec::new();
        pane.emit_to(
            &["✓ [1/3] image  api  cached  8ms".to_owned()],
            "⠋ Booting demo",
            &mut out,
        );
        let text = String::from_utf8(out).expect("UTF-8");
        assert_eq!(
            text, "✓ [1/3] image  api  cached  8ms\n\r\x1b[2K⠋ Booting demo",
            "an unpainted pane prints the row then repaints exactly one frame: {text:?}"
        );
        assert_no_forbidden(&text);

        let mut next = Vec::new();
        pane.emit_to(
            &["✓ [2/3] setup  uv sync  done  1ms".to_owned()],
            "⠙ Booting demo",
            &mut next,
        );
        let text = String::from_utf8(next).expect("UTF-8");
        assert!(
            text.starts_with("\r\x1b[2K✓ [2/3] setup  uv sync  done  1ms\n\r\x1b[2K"),
            "a painted status line is erased once before the row and repainted after: {text:?}"
        );
        assert_no_forbidden(&text);
    }

    #[test]
    fn every_carriage_return_is_immediately_followed_by_erase_line() {
        let mut pane = Pane::new(80);
        let mut out = Vec::new();
        for row in ["✓ one", "✓ two", "✓ three"] {
            pane.emit_to(&[row.to_owned()], "⠋ Booting demo", &mut out);
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
        let mut pane = Pane::new(80);
        let mut out = Vec::new();
        pane.emit_to(&["✓ row".to_owned()], "⠋ Booting demo", &mut out);
        let mut erased = Vec::new();
        pane.erase_to(&mut erased);
        assert_eq!(erased, b"\r\x1b[2K", "one erase prefix through the sink");
        let mut again = Vec::new();
        pane.erase_to(&mut again);
        assert!(again.is_empty(), "a second erase writes nothing: {again:?}");
    }
}
