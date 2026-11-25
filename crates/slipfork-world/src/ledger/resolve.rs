//! Target resolution over ledger rows (`specs/03` §7.3): family-scoped inside a checkout,
//! unique-or-fail-closed outside it. No `.forks` directory is ever re-scanned here.

use std::collections::BTreeMap;

use super::converge::{ForkRow, WorldRow};
use super::error::{LedgerError, LedgerResult};
use crate::layout::{ProjectScope, WorldName};

/// One resolved target: a world row plus its `forks` row when it is a fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The `worlds` row.
    pub world: WorldRow,
    /// The lineage row, `None` for a root.
    pub fork: Option<ForkRow>,
}

/// Resolves `needle` against the rows the caller already listed.
///
/// Inside a family `(root_id, name)` wins; outside, a bare `<name>` must match exactly one
/// world, otherwise the call fails closed with a `slipfork drop <world>` remedy.
pub fn resolve_target(
    scope: &ProjectScope,
    worlds: &[WorldRow],
    forks: &[ForkRow],
    needle: &str,
) -> LedgerResult<Option<Resolved>> {
    if WorldName::parse(needle).is_err() {
        return Err(LedgerError::UnknownTarget {
            needle: needle.to_owned(),
        });
    }
    let Some(root_id) = family_root(scope, worlds, forks) else {
        return outside_family(worlds, forks, needle);
    };
    if let Some(world) = worlds
        .iter()
        .find(|world| world.id == root_id && world.world_name == needle)
    {
        return Ok(Some(Resolved {
            world: world.clone(),
            fork: None,
        }));
    }
    let Some(fork) = forks
        .iter()
        .find(|fork| fork.root_id == root_id && fork.name == needle)
    else {
        return Err(LedgerError::UnknownTarget {
            needle: needle.to_owned(),
        });
    };
    let world = worlds
        .iter()
        .find(|world| world.id == fork.world_id)
        .ok_or_else(|| LedgerError::UnknownTarget {
            needle: needle.to_owned(),
        })?;
    Ok(Some(Resolved {
        world: world.clone(),
        fork: Some(fork.clone()),
    }))
}

fn family_root(scope: &ProjectScope, worlds: &[WorldRow], forks: &[ForkRow]) -> Option<i64> {
    let row = match scope {
        ProjectScope::None => return None,
        ProjectScope::Root { root, world, .. } => worlds
            .iter()
            .find(|row| row.world_name == *world || row.checkout_path == *root),
        ProjectScope::Fork { world, path, .. } => worlds
            .iter()
            .find(|row| row.world_name == *world)
            .or_else(|| {
                let (parent, _fork) = crate::layout::fork_checkout(path)?;
                worlds.iter().find(|row| row.checkout_path == parent)
            }),
    }?;
    Some(
        forks
            .iter()
            .find(|fork| fork.world_id == row.id)
            .map_or(row.id, |fork| fork.root_id),
    )
}

fn outside_family(
    worlds: &[WorldRow],
    forks: &[ForkRow],
    needle: &str,
) -> LedgerResult<Option<Resolved>> {
    let mut matches: BTreeMap<i64, (&WorldRow, Option<&ForkRow>)> = BTreeMap::new();
    for world in worlds {
        if world.world_name == needle {
            matches.insert(world.id, (world, None));
        }
    }
    for fork in forks {
        if fork.name != needle {
            continue;
        }
        if let Some(world) = worlds.iter().find(|world| world.id == fork.world_id) {
            matches.insert(world.id, (world, Some(fork)));
        }
    }
    match matches.len() {
        0 => Err(LedgerError::UnknownTarget {
            needle: needle.to_owned(),
        }),
        1 => {
            let (world, fork) = matches.into_values().next().expect("one match");
            Ok(Some(Resolved {
                world: world.clone(),
                fork: fork.cloned(),
            }))
        }
        _ => {
            let count = matches.len();
            let mut forks: Vec<String> = matches
                .into_values()
                .filter_map(|(world, fork)| fork.map(|_| world.world_name.clone()))
                .filter(|name| name.contains("--"))
                .collect();
            forks.sort();
            Err(LedgerError::AmbiguousTarget {
                needle: needle.to_owned(),
                count,
                forks,
            })
        }
    }
}
