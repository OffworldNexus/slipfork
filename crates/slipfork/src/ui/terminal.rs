//! One normalized terminal-size rule for the composition root and the live pane.

/// Narrowest usable terminal: a zero-column probe (a `script`/PTY with no inherited winsize)
/// reads as 80 columns, so no gutter ever wraps at eight characters.
pub const FALLBACK_COLS: usize = 80;

/// Normalizes one raw `(cols, rows)` probe.
///
/// Zero columns becomes [`FALLBACK_COLS`]; zero rows stays zero, which every caller reads as
/// "no anchored region" and falls back to the append-only path.
#[must_use]
pub const fn normalize_size(cols: usize, rows: usize) -> (usize, usize) {
    (if cols == 0 { FALLBACK_COLS } else { cols }, rows)
}

#[cfg(test)]
mod tests {
    use super::normalize_size;

    #[test]
    fn a_zero_size_probe_reads_as_eighty_columns_and_no_region() {
        assert_eq!(normalize_size(0, 0), (80, 0));
        assert_eq!(normalize_size(0, 32), (80, 32));
        assert_eq!(normalize_size(120, 32), (120, 32));
        assert_eq!(normalize_size(1, 0), (1, 0));
    }
}
