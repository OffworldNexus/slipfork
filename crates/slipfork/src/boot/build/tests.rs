//! Test modules for the boot narrative: role-byte pins and layout/order behavior.

use super::{Columns, LiveKey, Phase};

mod failure;
mod layout;
mod order;
mod roles;

/// The shared `RUN api` phase at `[2/4]`, used by both test modules.
fn phase(columns: Columns) -> Phase {
    Phase::new(
        LiveKey::Boot {
            kind: "image".to_owned(),
            name: "api".to_owned(),
        },
        Some((2, 4)),
        "RUN".to_owned(),
        "api".to_owned(),
        "sh -c 'echo hi'".to_owned(),
        columns,
    )
}
