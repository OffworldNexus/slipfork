//! Sealing of resolved component environments to the subscription's ephemeral HPKE key.

use std::collections::BTreeMap;

use slipfork_forkfile::TaintedString;
use slipfork_ospp::OsppError;
use slipfork_ospp::wire::WireValue;

use super::frames::{FilledRng, seal_secret_with_rng};

/// Seal every tainted value and pass plaintext through, exactly as the production server does.
pub(super) fn seal_components(
    resolved: &BTreeMap<String, BTreeMap<String, TaintedString>>,
    public_key_hex: &str,
    rng: &mut FilledRng,
) -> Result<BTreeMap<String, BTreeMap<String, WireValue>>, OsppError> {
    let mut components = BTreeMap::new();
    for (component, variables) in resolved {
        let mut values = BTreeMap::new();
        for (name, value) in variables {
            let value = if value.is_tainted() {
                WireValue::Encrypted(seal_secret_with_rng(
                    public_key_hex,
                    component,
                    name,
                    value.expose(),
                    rng,
                )?)
            } else {
                WireValue::Plain(value.expose().to_owned())
            };
            values.insert(name.clone(), value);
        }
        components.insert(component.clone(), values);
    }
    Ok(components)
}
