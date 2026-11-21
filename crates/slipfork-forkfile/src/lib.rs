//! The `Forkfile` language front end: parse, typecheck, evaluate, `migrate`, and `fmt`.

/// Entry marker for the `Forkfile` compiler owned by this crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ForkfileCompiler;

impl ForkfileCompiler {
    /// Builds the compiler marker.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}
