//! The daemon-owned world head and its durable `Lease` (`specs/03-under-the-hood.md` §6.3).
//!
//! The lease is the only owner of "is this world current": `WorldStamp` still carries the
//! hook/PG effect digests, but currentness is `lease.head == planned head`. A planned lease is
//! derived from the sealed `WorldHeads` image keys, so the world node is never synthetic: gc pins
//! exactly the image/seed entries the booted world runs on. No network call and no build happens
//! here; every input is already on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use slipfork_forkfile::FeatureSet;
use slipfork_forkfile::ast::Forkfile;
use slipfork_world::{
    BtrfsEngine, GC_GRACE_MS, LEASE_FILE, Lease, LeaseEntry, NodeKey, Pin, SnapshotStore,
    StoreLayout, WorldName, WorldPlan, plan_epoch,
};

use crate::daemon::Failure;
use crate::world::features::WorldFeatures;
use crate::world::{HostEnv, WorldHeads};

/// One desired head: the canonical spec digest, the `world` key, and the paired `world_epoch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Head {
    pub(crate) spec: String,
    pub(crate) head: NodeKey,
    pub(crate) world_epoch: String,
}

/// A preplanned head plus the exact lease its boot must persist verbatim.
pub(crate) type Planned = (Head, Lease);

/// `<store>/worlds/<world>/.slipfork-lease.json`.
pub(crate) fn path(store_root: &Path, world: &WorldName) -> PathBuf {
    StoreLayout::at(store_root.to_path_buf())
        .world(world)
        .join(LEASE_FILE)
}

/// Compiles the checkout and derives its stable head spec for `world` from every on-disk input.
///
/// This never reads `WorldHeads`, so it is the cheap drift probe a boot re-runs at `ready`.
pub(crate) fn desired_for(project: &Path, world: &WorldName) -> Result<Head, Failure> {
    let (doc, features, plan) = compile(project)?;
    Ok(spec_head(project, world, &doc, &features, &plan))
}

/// Reads the sealed `WorldHeads` and pairs the head spec with a lease over their image keys.
pub(crate) fn prepare(
    host: &HostEnv,
    project: &Path,
    world: &WorldName,
) -> Result<Planned, Failure> {
    let Head {
        spec, world_epoch, ..
    } = desired_for(project, world)?;
    let heads = WorldHeads::load(&WorldHeads::path(&host.runtime_dir, world))
        .map_err(|message| Failure::new("image-heads", message))?;
    // Every sealed image is one service slot over its own key; no seed head is recorded yet, so
    // `seeds` stays empty until a PG `initdb` node is sealed into `WorldHeads`.
    let services: BTreeMap<String, LeaseEntry> = heads
        .images
        .iter()
        .map(|(id, image)| {
            (
                format!("image:{id}"),
                LeaseEntry {
                    key: image.key,
                    image: image.key,
                },
            )
        })
        .collect();
    let mut lease = Lease {
        v: 1,
        world: world.as_str().to_owned(),
        origin: origin(world),
        world_epoch,
        spec: spec.clone(),
        head: NodeKey::from_digest([0; 32]),
        seeds: Vec::new(),
        services,
        effects: BTreeMap::new(),
    };
    lease.head = lease.derived_head();
    let head = Head {
        spec,
        head: lease.head,
        world_epoch: lease.world_epoch.clone(),
    };
    Ok((head, lease))
}

/// Compiles the checkout into its `Forkfile`, resolved features, and compiled world plan.
fn compile(project: &Path) -> Result<(Forkfile, FeatureSet, WorldPlan), Failure> {
    let source = fs::read_to_string(project.join("Forkfile")).map_err(|error| {
        Failure::new(
            "forkfile",
            format!(
                "cannot read `Forkfile`: {error}\nnext: create a Forkfile, then retry `slipfork up`"
            ),
        )
    })?;
    let checked = slipfork_forkfile::compile(&source).map_err(|diagnostics| {
        let diagnostic = diagnostics.first();
        Failure::new(
            "invalid-forkfile",
            format!(
                "Forkfile:{}: {}\nnext: fix the first error, then retry `slipfork up`",
                diagnostic.map_or(0, |item| item.span.line()),
                diagnostic.map_or("the Forkfile is invalid", |item| item.message.as_str())
            ),
        )
    })?;
    let features = WorldFeatures::load_resolved(project, &checked.document).map_err(|message| {
        Failure::new(
            "feature",
            format!("{message}\nnext: {}", WorldFeatures::REMEDY),
        )
    })?;
    let plan = slipfork_world::world_plan_with(&checked.document, project, &features).map_err(
        |error| {
            Failure::new(
                "forkfile-plan",
                format!("{error}\nnext: fix the Forkfile, then run `slipfork doctor`"),
            )
        },
    )?;
    Ok((*checked.document, features, plan))
}

/// Hashes the canonical `Forkfile`, the resolved features, the compiled world plan, and every
/// image's `Dockerfile` + copied-context digest into one lowercase hex sha256.
fn spec_head(
    project: &Path,
    world: &WorldName,
    doc: &Forkfile,
    features: &FeatureSet,
    plan: &WorldPlan,
) -> Head {
    let inputs = crate::world::image_inputs(project, doc, &plan.images, features);
    let world_epoch = plan_epoch(plan, features, &inputs);
    let canonical = slipfork_forkfile::format(doc);
    let mut seed = format!(
        "slipfork-world-head-v1\0world={}\0epoch={world_epoch}\0canonical={canonical}\0",
        world.as_str()
    );
    for (image, digest) in &inputs {
        seed.push_str(&format!("image|{image}|{digest}\n"));
    }
    let spec = hex::encode(Sha256::digest(seed.as_bytes()));
    Head {
        head: NodeKey::world(&[], &spec),
        spec,
        world_epoch,
    }
}

/// Writes the lease beside its world with tmp + rename; a missing world fails closed.
pub(crate) fn write(store_root: &Path, world: &WorldName, lease: &Lease) -> Result<(), Failure> {
    let world_dir = StoreLayout::at(store_root.to_path_buf()).world(world);
    if !world_dir.is_dir() {
        return Err(Failure::new(
            "world-state",
            format!(
                "world data `{}` is missing\nnext: run `slipfork down`, then `slipfork up`",
                world_dir.display()
            ),
        ));
    }
    lease
        .write_atomic(&path(store_root, world))
        .map_err(|error| {
            Failure::new(
                "lease-write",
                format!("cannot write the world lease: {error}\nnext: retry `slipfork up`"),
            )
        })
}

/// Reads a world's lease; a missing or malformed file is simply "no current head".
pub(crate) fn read(store_root: &Path, world: &WorldName) -> Option<Lease> {
    Lease::read(&path(store_root, world)).ok()
}

/// Removes a lease a failed boot, rollback, or detached world must never leave behind.
pub(crate) fn remove(store_root: &Path, world: &WorldName) {
    let _ = fs::remove_file(path(store_root, world));
}

/// Sweeps unreachable snapshot nodes after a successful plan (24h grace).
///
/// The entry image/seed keys and their record closure are pinned; the synthetic world key is
/// never pinned. A gc error is returned so the caller can trace it instead of hiding it.
pub(crate) fn collect_garbage(
    host: &HostEnv,
    project: &Path,
    lease: Option<&Lease>,
) -> Result<(), String> {
    let root = host.store_root_for(project);
    let layout = StoreLayout::at(root.clone());
    if !layout.snapshots_root().is_dir() {
        return Ok(());
    }
    let engine = BtrfsEngine::new(StoreLayout::at(root));
    let store = SnapshotStore::new(&layout, &engine);
    let pins: Vec<Pin> = lease
        .map(|lease| {
            lease
                .gc_mark()
                .into_iter()
                .map(|key| Pin::new(key, BTreeSet::from([key])))
                .collect()
        })
        .unwrap_or_default();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        });
    store
        .gc(now_ms, GC_GRACE_MS, &pins)
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// `<fork>--<world>` leases its parent world as `origin`; every other world is its own origin.
fn origin(world: &WorldName) -> String {
    StoreLayout::parse_fork_world(OsStr::new(world.as_str())).map_or_else(
        || world.as_str().to_owned(),
        |(_, parent)| parent.as_str().to_owned(),
    )
}

impl crate::daemon::lifecycle::core::Core {
    /// Deletes a lease whose world failed to boot, was detached, or was dropped.
    pub(crate) fn remove_lease(&self, project: &Path, world: &str) {
        if let Ok(world) = WorldName::parse(world) {
            remove(&self.host().store_root_for(project), &world);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use slipfork_world::{Lease, NodeKey, WorldName};

    use super::{desired_for, read, remove, write};
    use crate::world::features::WorldFeatures;

    const FORKFILE: &str = "# forkfile: 0.1\n\nfeatures {\n  dev\n  mail\n}\n\nimage app . {\n  dockerfile Dockerfile\n  service web {\n    port 8080\n    route /\n    run: sleep 3600\n  }\n}\n";

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    fn project(prefix: &str) -> (tempfile::TempDir, WorldName) {
        let root = temp(prefix);
        fs::write(root.path().join("Forkfile"), FORKFILE).expect("write Forkfile");
        fs::write(
            root.path().join("Dockerfile"),
            "FROM scratch\nCOPY seed.txt /seed.txt\n",
        )
        .expect("write Dockerfile");
        fs::write(root.path().join("seed.txt"), "one").expect("write the copied context");
        (root, WorldName::parse("demo").expect("valid world name"))
    }

    /// An unchanged tree re-plans to the exact same head, so `up` stays Idle.
    #[test]
    fn an_unchanged_plan_keeps_the_same_head() {
        let (root, world) = project("slipfork-head-stable");
        let first = desired_for(root.path(), &world).expect("head the project");
        assert_eq!(
            first,
            desired_for(root.path(), &world).expect("re-head the project"),
            "an unchanged plan keeps its head"
        );
    }

    /// A `Dockerfile` or copied-context edit changes the head and forces a rebuild.
    #[test]
    fn dockerfile_and_copied_context_edits_change_the_head() {
        let (root, world) = project("slipfork-head-images");
        let first = desired_for(root.path(), &world).expect("head the project");
        fs::write(
            root.path().join("Dockerfile"),
            "FROM scratch\nCOPY seed.txt /seed.txt\nRUN echo two\n",
        )
        .expect("edit the Dockerfile");
        let edited = desired_for(root.path(), &world).expect("head after the edit");
        assert_ne!(first, edited, "a Dockerfile edit must change the head");
        fs::write(root.path().join("seed.txt"), "two").expect("edit the copied context");
        assert_ne!(
            edited,
            desired_for(root.path(), &world).expect("head after the context edit"),
            "a copied-context edit must change the head"
        );
    }

    /// A feature-set or route edit changes the head without touching a Dockerfile.
    #[test]
    fn feature_and_route_edits_change_the_head() {
        let (root, world) = project("slipfork-head-decl");
        let first = desired_for(root.path(), &world).expect("head the project");
        WorldFeatures::new(true, Vec::new())
            .save(root.path())
            .expect("persist a feature reset");
        let featured = desired_for(root.path(), &world).expect("head after the feature edit");
        assert_ne!(first, featured, "a feature edit must change the head");
        fs::write(
            root.path().join("Forkfile"),
            FORKFILE.replace("route /", "route /api"),
        )
        .expect("edit the route");
        assert_ne!(
            featured,
            desired_for(root.path(), &world).expect("head after the route edit"),
            "a route edit must change the head"
        );
    }

    /// `.git` is build-tool state: it never enters a head.
    #[test]
    fn git_metadata_changes_never_change_the_head() {
        let (root, world) = project("slipfork-head-git");
        let first = desired_for(root.path(), &world).expect("head the project");
        fs::create_dir_all(root.path().join(".git")).expect("create .git");
        fs::write(root.path().join(".git/HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        assert_eq!(
            first,
            desired_for(root.path(), &world).expect("head after the git edit"),
            "only `.git` changed, so the head is stable"
        );
    }

    /// A fork world has its own head even when it shares the parent's bytes.
    #[test]
    fn a_fork_world_head_differs_from_its_parent() {
        let (root, parent) = project("slipfork-head-fork");
        let fork = WorldName::parse("feat-auth--demo").expect("valid fork world");
        assert_ne!(
            desired_for(root.path(), &parent).expect("parent head"),
            desired_for(root.path(), &fork).expect("fork head"),
            "the fork's world id is part of its head"
        );
    }

    /// The lease round-trips atomically, derives its own world key, and is deleted on rollback.
    #[test]
    fn a_lease_round_trips_and_is_deleted() {
        let (root, world) = project("slipfork-head-lease");
        let store = root.path().join("store");
        fs::create_dir_all(store.join("worlds").join(world.as_str()))
            .expect("create the world dir");
        let desired = desired_for(root.path(), &world).expect("head the project");
        let image = NodeKey::from_digest([7; 32]);
        let mut lease = Lease {
            v: 1,
            world: world.as_str().to_owned(),
            origin: world.as_str().to_owned(),
            world_epoch: desired.world_epoch,
            spec: desired.spec,
            head: NodeKey::from_digest([0; 32]),
            seeds: Vec::new(),
            services: BTreeMap::from([(
                "image:app".to_owned(),
                slipfork_world::LeaseEntry { key: image, image },
            )]),
            effects: BTreeMap::new(),
        };
        lease.head = lease.derived_head();
        write(&store, &world, &lease).expect("write the lease");
        let read_back = read(&store, &world).expect("read the lease back");
        assert_eq!(read_back, lease);
        assert_eq!(read_back.world, world.as_str());
        assert_eq!(read_back.origin, world.as_str());
        assert!(
            read_back.is_consistent(),
            "the daemon-written lease derives its own head"
        );
        remove(&store, &world);
        assert!(read(&store, &world).is_none(), "rollback deletes the lease");
    }
}
