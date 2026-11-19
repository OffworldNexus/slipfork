//! Base snapshot identity: backing-service versions plus every resolved dump digest.

use sha2::{Digest, Sha256};
use slipfork_dag::{PgSeed, ResourcePlan};
use slipfork_edge::EdgeError;
use slipfork_engine::elapsed_millis;
use slipfork_forkfile::EvaluatedFork;
use slipfork_world::BaseLineage;
use slipfork_world::SeedLineage;
use slipfork_world::rootfs::WorldRootfs;
use slipfork_world::{
    BootLineage, EngineError, ExecEnv, ProjectSlug, SnapshotId, WorldSpec, WorldTarget,
};

use super::backing::requested_service_images;
use super::runtime_error;
use super::service::DaemonService;
use super::types::SealedBase;
use crate::layout::ensure_private_dir;

/// Content key of one base snapshot: backing-service versions plus every resolved dump digest.
/// The seed dumps the plan selected: `(any_seed, single_digest)`.
///
/// `false` means the plan restores no dump (a clean `initdb` base); several dumps are a seed with
/// no single digest, so the digest stays `None` rather than naming one database arbitrarily.
pub(super) fn selected_seed(fork: &EvaluatedFork, plan: &ResourcePlan) -> (bool, Option<String>) {
    let declared: Vec<String> = fork
        .pg_databases()
        .into_iter()
        .map(|database| database.database)
        .collect();
    let has_seed = plan.pg_databases.iter().any(|database| {
        declared.contains(&database.id) && matches!(database.seed, PgSeed::RemoteDump { .. })
    });
    // OSPP v1 no longer advertises a dump digest, so the only honest lineage label is `remote`.
    (has_seed, has_seed.then(|| "remote".to_owned()))
}

/// The facts a world reuses when the base snapshot already existed on disk.
///
/// No seal ran for this world (`base.millis: None`) and no seed was rebuilt
/// (`seed.rebuilt: Some(false)`), so a restarted daemon never claims another boot's duration.
pub(super) fn reused_base_lineage(
    key: &str,
    has_seed: bool,
    digest_hex: Option<String>,
) -> BootLineage {
    BootLineage {
        base: Some(BaseLineage {
            key: key.to_owned(),
            millis: None,
        }),
        seed: has_seed.then_some(SeedLineage {
            digest: digest_hex,
            rebuilt: Some(false),
            millis: None,
        }),
    }
}

impl DaemonService {
    /// Seal the real `<project>-base-<digest>` snapshot (`ADR-0008` §2, `ADR-0009` §2–§3).
    ///
    /// A short-lived seed world boots on an empty root snapshot, starts every backing-service
    /// container, restores the selected dumps with `pg_restore` + `CHECKPOINT;` **inside** the
    /// `postgres:<v>` container, and is then frozen and destroyed. `up`/`fork` clone the sealed
    /// base and never re-run `pg_restore`; `--no-dump` seals the clean `initdb` base through the
    /// same path.
    ///
    /// Returns the snapshot plus the [`BootLineage`] the clone records on its own handle: a seal
    /// performed by this call reports its measured duration and seed rebuild verdict, while a base
    /// that already existed reports `base.millis: None` (rendered `reused`) and
    /// `seed.rebuilt: Some(false)` (rendered `cached`). Every duration comes from the daemon's one
    /// own plain [`std::time::Instant`] observations; nothing is estimated.
    pub(super) fn ensure_base_snapshot(
        &self,
        project: &ProjectSlug,
        fork: &EvaluatedFork,
        plan: &ResourcePlan,
        rootfs: &WorldRootfs,
    ) -> Result<(SnapshotId, BootLineage), EdgeError> {
        let digest = base_digest(project, fork, plan);
        let key = (project.clone(), digest.clone());
        if let Some(sealed) = self.lock().bases.get(&key).cloned() {
            return Ok((sealed.snapshot, sealed.lineage));
        }

        // Pre-flight: a base snapshot already on disk is reused as-is, so the seeding pipeline (seed
        // world boot, `pg_restore`, seal) never runs only to be discarded by `SnapshotExists`.
        let snapshot = SnapshotId(format!("{project}-base-{digest}"));
        let reused = self
            .engine
            .snapshot_exists(&snapshot)
            .map_err(|err| runtime_error(format!("cannot look up the base snapshot: {err}")))?;
        if reused {
            let (has_seed, digest_hex) = selected_seed(fork, plan);
            let lineage = reused_base_lineage(&digest, has_seed, digest_hex);
            self.lock().bases.insert(
                key,
                SealedBase {
                    snapshot: snapshot.clone(),
                    lineage: lineage.clone(),
                },
            );
            return Ok((snapshot, lineage));
        }

        let seed = self.config.layout.base_seed_dir(project);
        ensure_private_dir(&seed).map_err(runtime_error)?;
        let empty = SnapshotId(format!("{project}-seed-empty"));
        // A sealed snapshot is immutable: an existing empty seed is reused as-is, never re-sealed.
        match self.engine.create_snapshot(&seed, &empty) {
            Ok(()) | Err(EngineError::SnapshotExists { .. }) => {}
            Err(err) => {
                return Err(runtime_error(format!(
                    "cannot create the empty base seed snapshot: {err}"
                )));
            }
        }

        let spec = WorldSpec {
            target: WorldTarget::TestSlot(0),
            rootfs: rootfs.clone(),
            snapshot: empty,
            mounts: Vec::new(),
            base_env: ExecEnv::world_scoped(self.config.layout.world_home(project, "base-seed")),
            local_ingress: None,
            boot: BootLineage::default(),
        };
        let handle = self.engine.clone_world(&spec).map_err(|err| {
            runtime_error(format!(
                "cannot boot the base seed world for '{project}': {err}"
            ))
        })?;

        let restore_started = std::time::Instant::now();
        let restored = self.restore_base(project, &handle, fork, plan, rootfs);
        let restore_millis = restored
            .as_ref()
            .ok()
            .map(|()| elapsed_millis(restore_started, std::time::Instant::now()));
        // The seal outcome decides the facts: only a seal performed here may claim a duration.
        let seal_started = std::time::Instant::now();
        let sealed = restored.and_then(|()| match self.engine.freeze_world(&handle, &snapshot) {
            Ok(()) => Ok(true),
            // The base is already sealed on disk: the world reuses it and claims no seal time.
            Err(EngineError::SnapshotExists { .. }) => Ok(false),
            Err(err) => Err(runtime_error(format!(
                "cannot freeze the base seed world: {err}"
            ))),
        });
        let sealed_now = match sealed {
            Ok(sealed_now) => sealed_now,
            Err(error) => {
                let _ = self.engine.destroy_world(handle);
                return Err(error);
            }
        };
        let seal_millis =
            sealed_now.then(|| elapsed_millis(seal_started, std::time::Instant::now()));
        self.engine
            .destroy_world(handle)
            .map_err(|err| runtime_error(format!("cannot destroy the base seed world: {err}")))?;
        // A seed exists only when a dump was restored; several dumps have no single digest.
        let (has_seed, digest_hex) = selected_seed(fork, plan);
        let seed = has_seed.then_some(SeedLineage {
            digest: digest_hex,
            rebuilt: Some(sealed_now),
            millis: if sealed_now { restore_millis } else { None },
        });
        let facts = BootLineage {
            base: Some(BaseLineage {
                key: digest.clone(),
                millis: seal_millis,
            }),
            seed,
        };
        let reused_facts = reused_base_lineage(
            &digest,
            has_seed,
            facts.seed.as_ref().and_then(|seed| seed.digest.clone()),
        );
        self.lock().bases.insert(
            key,
            SealedBase {
                snapshot: snapshot.clone(),
                lineage: reused_facts,
            },
        );
        Ok((snapshot, facts))
    }
}

pub(super) fn base_digest(
    project: &ProjectSlug,
    fork: &EvaluatedFork,
    plan: &ResourcePlan,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"slipfork-base:v1\0");
    hasher.update(project.as_str().as_bytes());
    hasher.update(b"\0");
    for image in requested_service_images(fork) {
        hasher.update(image.to_string().as_bytes());
        hasher.update(b"\0");
    }
    for database in &plan.pg_databases {
        hasher.update(database.id.as_bytes());
        hasher.update(b"=");
        match &database.seed {
            PgSeed::CleanInitdb => hasher.update(b"clean"),
            PgSeed::RemoteDump { .. } => hasher.update(b"remote"),
        }
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
}
