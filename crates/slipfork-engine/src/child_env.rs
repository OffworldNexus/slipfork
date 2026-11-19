//! Composition of the child-process environment at the adapter boundary.
//!
//! [`slipfork_world::ExecEnv`] stays free of ambient state. [`InheritedEnv`] carries the *inherited
//! allowlist* (`PATH`, `TERM`, `LANG`) that the composition root resolved from its own process
//! environment once, at construction; the engine never reads `std::env`. Values explicitly
//! recorded in the effective [`slipfork_world::ExecEnv`] always win, and no variable outside
//! [`InheritedVar::ALL`] is ever inherited.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use slipfork_world::{ExecEnv, InheritedVar};

/// Ambient allowlist values the composition root resolved from its own process environment.
///
/// The library never reads `std::env`: [`crate::BtrfsEngine::new`] receives this value, and it is
/// the only source of the inherited `PATH`/`TERM`/`LANG` values handed to world processes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InheritedEnv {
    values: BTreeMap<String, String>,
}

impl InheritedEnv {
    /// Build from explicitly supplied `name=value` pairs; the composition root reads them.
    pub fn new(values: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    /// The value of one allowlisted variable, as resolved by the composition root.
    pub fn get(&self, var: InheritedVar) -> Option<String> {
        self.values.get(var.name()).cloned()
    }
}

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

/// Effective environment of one OCI container: the inherited allowlist overridden by the
/// container's **own** declared environment only.
///
/// This is the bundle composition: the writer persists exactly this map into
/// `<ctl>/oci*/<id>/config.json`, and a container `exec` re-derives from the same
/// `ServiceSpec::env` base with its own overlay. Neither path can pick up the world component
/// environment (`WorldSpec::base_env`, every resolved `modes.*.envs.*` secret included).
pub(crate) fn materialize_container(
    own: &ExecEnv,
    ambient: impl Fn(InheritedVar) -> Option<String>,
) -> BTreeMap<String, String> {
    materialize(&ExecEnv::default(), own, ambient)
}
