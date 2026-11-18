//! Composition of the child-process environment at the adapter boundary.
//!
//! [`slipfork_world::ExecEnv`] stays free of ambient state. Each adapter supplies the *inherited allowlist*
//! lookup (`PATH`, `TERM`, `LANG`) from its own process environment — the same way every adapter
//! reads OS state — while values explicitly recorded in the effective [`slipfork_world::ExecEnv`] always
//! win. No variable outside [`InheritedVar::ALL`] is ever inherited.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use slipfork_world::{ExecEnv, InheritedVar};

/// Effective child environment: `ambient` allowlist overridden by `base ⊕ overlay`.
pub(crate) fn materialize(
    base: &ExecEnv,
    overlay: &ExecEnv,
    ambient: impl Fn(InheritedVar) -> Option<String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for var in InheritedVar::ALL {
        if let Some(value) = ambient(var) {
            env.insert(var.name().to_owned(), value);
        }
    }
    env.extend(base.overlaid(overlay).expose_to_child_process());
    env
}
