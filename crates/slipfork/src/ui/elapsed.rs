//! The one human spelling of a duration, shared by every CLI surface.

/// `18ms`, `1.2s`, or `<1ms`, as the whole CLI spells durations.
#[must_use]
pub fn elapsed_text(millis: u64) -> String {
    if millis == 0 {
        "<1ms".to_owned()
    } else if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", millis as f64 / 1000.0)
    }
}
