//! `OSPP v1` frames, device keys, HPKE sealing, and dumps.

/// Entry marker for the OSPP endpoint owned by this crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OsppEndpoint;

impl OsppEndpoint {
    /// Builds the endpoint marker.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}
