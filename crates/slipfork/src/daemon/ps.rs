//! The one wire schema of `ps`: the daemon serializes it and the CLI renderer deserializes it.
//!
//! Nothing downstream inspects `serde_json::Value` keys or sniffs authority strings: every field is
//! typed here, including the `Forkfile` domain filter mirror.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use slipfork_forkfile::BucketPolicy;
use slipfork_world::LedgerRows;

/// The whole `ps` reply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsReply {
    /// Every owned world, with forks nested under their parent.
    #[serde(default)]
    pub worlds: Vec<PsWorld>,
}

/// Nests the already-built rows into the reply tree from the ledger's parent edges.
///
/// A fork sits under its immediate parent row, or under its root row when the parent row is gone;
/// a cycle (which the ledger never writes) still surfaces every row as a remainder. The daemon and
/// the daemonless reader both build through this one projection, so their trees cannot drift.
#[must_use]
pub fn assemble_reply(rows: &LedgerRows, mut built: BTreeMap<i64, PsWorld>) -> PsReply {
    let known: BTreeSet<i64> = rows.worlds.iter().map(|row| row.id).collect();
    let mut children: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for fork in &rows.forks {
        if !built.contains_key(&fork.world_id) {
            continue;
        }
        let parent = if known.contains(&fork.parent_id) {
            fork.parent_id
        } else if known.contains(&fork.root_id) {
            fork.root_id
        } else {
            continue;
        };
        children.entry(parent).or_default().push(fork.world_id);
    }
    let nested: BTreeSet<i64> = children.values().flatten().copied().collect();
    let mut worlds = Vec::new();
    for row in &rows.worlds {
        if !nested.contains(&row.id)
            && let Some(world) = take(row.id, &children, &mut built)
        {
            worlds.push(world);
        }
    }
    while let Some(id) = built.keys().next().copied() {
        if let Some(world) = take(id, &children, &mut built) {
            worlds.push(world);
        }
    }
    PsReply { worlds }
}

/// Takes one assembled subtree out of `built`, nesting its children in project order.
fn take(
    id: i64,
    children: &BTreeMap<i64, Vec<i64>>,
    built: &mut BTreeMap<i64, PsWorld>,
) -> Option<PsWorld> {
    let mut world = built.remove(&id)?;
    if let Some(kids) = children.get(&id) {
        for &kid in kids {
            if let Some(child) = take(kid, children, built) {
                world.forks.push(child);
            }
        }
    }
    world
        .forks
        .sort_by(|left, right| left.project.cmp(&right.project));
    Some(world)
}

/// The one `!` line for a checkout the ledger still owns but disk no longer has.
#[must_use]
pub fn checkout_problem(fork: Option<&str>, world: &str) -> String {
    match fork {
        Some(fork) => format!("checkout missing — run `slipfork drop {fork}`"),
        None => format!("checkout missing — restore it or run `slipfork down {world}`"),
    }
}

/// Appends exactly one remedy when a ledger's broken-row text stops at the problem.
#[must_use]
pub fn with_remedy(problem: &str, fork: Option<&str>, world: &str) -> String {
    if problem.contains("slipfork ") {
        return problem.to_owned();
    }
    let remedy = match fork {
        Some(fork) => format!("run `slipfork drop {fork}`"),
        None => format!("restore it or run `slipfork down {world}`"),
    };
    format!("{problem} — {remedy}")
}

/// The ancestor row ids of `id`: the immediate parent first, the root row last.
///
/// A root row has no ancestors; a cycle (which the ledger never writes) terminates on the visited
/// set. Every `ps` lineage and every lease `origin` reads its ancestry through this one walk.
#[must_use]
pub fn ancestry(rows: &LedgerRows, id: i64) -> Vec<i64> {
    let mut chain = Vec::new();
    let mut parent = rows
        .forks
        .iter()
        .find(|fork| fork.world_id == id)
        .map(|fork| fork.parent_id);
    let mut seen = BTreeSet::from([id]);
    while let Some(id) = parent {
        if !seen.insert(id) {
            break;
        }
        chain.push(id);
        parent = rows
            .forks
            .iter()
            .find(|fork| fork.world_id == id)
            .map(|fork| fork.parent_id);
    }
    chain
}

/// The ancestor fork names of `id`: immediate parent first, the root world name last.
///
/// A root row has no lineage; an edge whose parent row vanished stops before naming it, and a
/// cycle (which the ledger never writes) terminates on the visited set.
#[must_use]
pub fn lineage(rows: &LedgerRows, id: i64) -> Vec<String> {
    ancestry(rows, id)
        .into_iter()
        .filter_map(|id| {
            if let Some(fork) = rows.forks.iter().find(|fork| fork.world_id == id) {
                return Some(fork.name.clone());
            }
            rows.worlds
                .iter()
                .find(|row| row.id == id)
                .map(|row| row.world_name.clone())
        })
        .collect()
}

/// One world or fork.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsWorld {
    /// Project directory the world was booted from.
    pub project: PathBuf,
    /// Project directory, as the status tree prints it.
    pub path: PathBuf,
    /// World id (`demo`, or `<fork>--<world>` for a fork).
    pub world: String,
    /// Fork name, when this world is a fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    /// Primary URL, `<world>.localhost`.
    pub url: String,
    /// First declared app `domain <id>`; it **is** the primary `<world>.localhost` authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_domain: Option<String>,
    /// Whether this world is live or only a stopped checkout on disk.
    #[serde(default)]
    pub state: PsWorldState,
    /// Milliseconds since the Unix epoch when the world became ready; absent on a stopped row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Supervisor process id; absent on a stopped row instead of a fabricated `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor_pid: Option<u32>,
    /// The supervisor still owns its control socket.
    pub alive: bool,
    /// The world is still booting.
    pub booting: bool,
    /// Declared services and resources, in declaration order.
    #[serde(default)]
    pub services: Vec<PsService>,
    /// Why this world cannot be fully described: a missing checkout or an invalid `Forkfile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// Ancestor fork names of this row, immediate parent first, the root world name last.
    #[serde(default)]
    pub lineage: Vec<String>,
    /// Forks nested under this world.
    #[serde(default)]
    pub forks: Vec<PsWorld>,
}

impl PsWorld {
    /// A project or fork checkout that still exists on disk but is not running.
    #[must_use]
    pub(crate) fn stopped(project: &Path, world: &str, fork: Option<&str>) -> Self {
        Self {
            project: project.to_path_buf(),
            path: project.to_path_buf(),
            world: world.to_owned(),
            fork: fork.map(str::to_owned),
            url: format!("http://{world}.localhost"),
            primary_domain: None,
            state: PsWorldState::Stopped,
            started_at_ms: None,
            supervisor_pid: None,
            alive: false,
            booting: false,
            services: Vec::new(),
            problem: None,
            lineage: Vec::new(),
            forks: Vec::new(),
        }
    }
}

/// Whether one `ps` world is live or a stopped checkout still on disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PsWorldState {
    /// A supervisor and its control socket answer for this world.
    #[default]
    Live,
    /// The checkout exists on disk but nothing runs for it.
    Stopped,
    /// The ledger and disk disagree; `problem` is the only honest row.
    Broken,
}

/// One service, image, or backing resource row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsService {
    /// Service, resource, or bucket id.
    pub id: String,
    /// Which kind of row this is.
    pub kind: PsKind,
    /// The tracked pod is alive.
    pub running: bool,
    /// Public port, when declared or implied by the resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Explicit or default backing version (`18`, `7`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// CDN authority attached to an S3 bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdn: Option<String>,
    /// Anonymous-read policy of an `s3` bucket; `None` for every other row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PsBucketPolicy>,
    /// Host-visible pod pid, when the world reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    /// Declared ingress routes.
    #[serde(default)]
    pub routes: Vec<PsRoute>,
    /// The tracked pod is alive but no longer declared by the current `Forkfile`.
    #[serde(default)]
    pub orphaned: bool,
    /// Enclosing `image <id>` for a service row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The service's declared `run:` command, shown in the status command column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// The closed set of `ps` row kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PsKind {
    /// A `service` pod.
    Service,
    /// An image without its own service pod.
    Image,
    /// A `postgres` resource.
    Postgres,
    /// A `redis` resource.
    Redis,
    /// An `s3` bucket.
    S3,
    /// A CDN authority.
    Cdn,
}

/// One declared ingress route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsRoute {
    /// Leading path prefix.
    pub prefix: String,
    /// Upstream port inside the world.
    pub port: u16,
    /// Prefix handling.
    pub mode: PsRouteMode,
    /// Authority filter, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// How a route treats its matched path prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PsRouteMode {
    /// Forward the path unchanged.
    Preserve,
    /// Strip the matched prefix.
    StripPrefix,
    /// Rewrite the prefix to another path.
    Rewrite,
    /// Match the prefix exactly.
    Exact,
}

/// The anonymous-read policy of one native S3 bucket, as `ps --json` exposes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PsBucketPolicy {
    /// Anonymous reads are refused with `403`.
    Private,
    /// Anonymous `GET`/`HEAD` succeed; writes still need SigV4.
    PublicRead,
}

impl From<BucketPolicy> for PsBucketPolicy {
    fn from(policy: BucketPolicy) -> Self {
        match policy {
            BucketPolicy::Private => Self::Private,
            BucketPolicy::PublicRead => Self::PublicRead,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use slipfork_world::{ForkRow, LedgerRows, WorldRow, WorldState};

    use super::{ancestry, lineage};

    fn row(id: i64, world: &str, checkout: &str) -> WorldRow {
        WorldRow {
            id,
            checkout_path: PathBuf::from(checkout),
            world_name: world.to_owned(),
            store_root: PathBuf::from("/var/tmp/slipfork-store"),
            state: WorldState::Stopped,
            supervisor_pid: None,
            pods_dir: None,
            started_at_ms: None,
            problem: None,
            updated_at_ms: 0,
        }
    }

    fn edge(world_id: i64, root_id: i64, parent_id: i64, name: &str) -> ForkRow {
        ForkRow {
            world_id,
            root_id,
            parent_id,
            name: name.to_owned(),
            branch: name.to_owned(),
        }
    }

    /// `salut` is a fork of `bonjour` under `ghostroot`: the immediate parent names first, the
    /// root world last, and a root row has no ancestors at all.
    #[test]
    fn lineage_names_ancestor_forks_then_the_root_world() {
        let rows = LedgerRows {
            worlds: vec![
                row(1, "ghostroot", "/p"),
                row(2, "bonjour--ghostroot", "/p/.forks/bonjour"),
                row(3, "salut--ghostroot", "/p/.forks/salut"),
            ],
            forks: vec![edge(2, 1, 1, "bonjour"), edge(3, 1, 2, "salut")],
        };
        assert_eq!(
            lineage(&rows, 3),
            vec!["bonjour".to_owned(), "ghostroot".to_owned()]
        );
        assert_eq!(lineage(&rows, 2), vec!["ghostroot".to_owned()]);
        assert!(lineage(&rows, 1).is_empty());
        assert_eq!(ancestry(&rows, 3), vec![2, 1]);
        assert_eq!(ancestry(&rows, 2), vec![1]);
        assert!(ancestry(&rows, 1).is_empty());
    }

    /// A corrupt edge that loops back on itself must terminate instead of hanging `ps`.
    #[test]
    fn lineage_stops_at_a_cycle() {
        let rows = LedgerRows {
            worlds: vec![row(1, "a--b", "/p/.forks/a"), row(2, "b--c", "/p/.forks/b")],
            forks: vec![edge(1, 1, 2, "a"), edge(2, 1, 1, "b")],
        };
        assert_eq!(lineage(&rows, 1), vec!["b".to_owned()]);
        assert_eq!(lineage(&rows, 2), vec!["a".to_owned()]);
    }
}
