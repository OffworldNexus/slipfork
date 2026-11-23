//! `world_epoch` checkpoints: `<world>/.slipfork-world-stamp.json` plus its paired PG sibling.
//!
//! A stamp is only reused when the world subvolume and every flat PG clone it was written with
//! carry the same epoch. `slipfork fork` snapshots both, so a cloned world skips every rebuild,
//! `@<tool>` install, and `before-start`/`pre-deploy` hook in 0ms; a mismatched pair forces the
//! hooks to run again.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use slipfork_forkfile::FeatureSet;
use slipfork_forkfile::boot::stamp_of;

use crate::boot::{BootKind, BootStep, HookPhase, WorldPlan};
use crate::error::{Error, Result};

/// The world checkpoint written at `<world>/.slipfork-world-stamp.json`.
pub const WORLD_STAMP_FILE: &str = ".slipfork-world-stamp.json";
/// The PG sibling checkpoint written at `worlds/<world>.pg.<pod>/.slipfork-pg-stamp.json`.
pub const PG_STAMP_FILE: &str = ".slipfork-pg-stamp.json";

/// One resolved image rootfs plus the runtime facts a fork reuses without rebuilding.
///
/// The parent stamp records these so a fork with a matching `world_epoch` adopts the already
/// sealed subvolume (`cached`, `~1ms`) instead of re-evaluating `Dockerfile` args — keeping e.g.
/// `arg VITE_CDN_URL = static-cdn.url` from changing the digest on every fork.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageStamp {
    /// Read-only subvolume the image resolved to.
    pub rootfs: String,
    /// Image `config.Env` entries inherited verbatim.
    #[serde(default)]
    pub env: Vec<String>,
    /// `ENTRYPOINT` followed by `CMD`.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Image `config.WorkingDir`.
    #[serde(default)]
    pub workdir: String,
}

/// Resolved world state plus the epoch shared with every resource clone at `CHECKPOINT`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldStamp {
    /// Epoch shared with every paired PG stamp.
    #[serde(default)]
    pub world_epoch: String,
    /// Resolved image rootfs + runtime facts per image id.
    #[serde(default)]
    pub images: BTreeMap<String, ImageStamp>,
    /// Completed `@<tool>` / `when changed(...)` digests.
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    /// `before-start` input + image/command digests.
    #[serde(default)]
    pub before_start: BTreeMap<String, String>,
    /// `pre-deploy` input + image/command digests.
    #[serde(default)]
    pub pre_deploy: BTreeMap<String, String>,
}

impl WorldStamp {
    /// Builds the checkpoint for a completed boot: epoch, resolved images, and step digests.
    #[must_use]
    pub fn checkpoint(
        plan: &WorldPlan,
        epoch: String,
        images: BTreeMap<String, ImageStamp>,
    ) -> Self {
        let mut stamp = Self {
            world_epoch: epoch,
            images,
            ..Self::default()
        };
        for (index, step) in plan.steps.iter().enumerate() {
            let Some(digest) = step_digest(step) else {
                continue;
            };
            let key = step_slot_key(&plan.steps, index);
            match step.phase {
                Some(HookPhase::PreDeploy) => {
                    stamp.pre_deploy.insert(key, digest);
                }
                Some(HookPhase::BeforeStart) => {
                    stamp.before_start.insert(key, digest);
                }
                Some(HookPhase::PostDeploy | HookPhase::FailedDeploy) => {}
                None => {
                    stamp.tools.insert(key, digest);
                }
            }
        }
        stamp
    }

    /// Whether `step` at `index` in `steps` matches its recorded `before-start`/`pre-deploy`
    /// digest in this checkpoint.
    #[must_use]
    pub fn matches_hook(&self, steps: &[BootStep], index: usize) -> bool {
        let Some(step) = steps.get(index) else {
            return false;
        };
        let Some(expected) = step_digest(step) else {
            return false;
        };
        let key = step_slot_key(steps, index);
        match step.phase {
            Some(HookPhase::BeforeStart) => self.before_start.get(&key) == Some(&expected),
            Some(HookPhase::PreDeploy) => self.pre_deploy.get(&key) == Some(&expected),
            None => self.tools.get(&key) == Some(&expected),
            Some(HookPhase::PostDeploy | HookPhase::FailedDeploy) => false,
        }
    }
}

/// Deterministic digest of one setup/hook step: its enclosing image, rendered command, and any
/// matched input-file Merkle digest.
#[must_use]
pub fn step_digest(step: &BootStep) -> Option<String> {
    if !matches!(step.kind, BootKind::Setup | BootKind::Hook) {
        return None;
    }
    let inputs = vec![
        (
            PathBuf::from("image"),
            step.image.as_ref().map(|value| value.as_bytes().to_vec()),
        ),
        (
            PathBuf::from("command"),
            step.command.as_ref().map(|value| value.as_bytes().to_vec()),
        ),
        (
            PathBuf::from("inputs"),
            step.stamp
                .as_ref()
                .map(|conditional| conditional.digest.as_bytes().to_vec()),
        ),
    ];
    Some(stamp_of(&inputs))
}

/// Stable map key for one setup/hook node; disambiguates multi-command blocks by ordinal.
fn step_slot_key(steps: &[BootStep], index: usize) -> String {
    let step = &steps[index];
    let base = match step.phase {
        Some(_) => step.node_name(),
        None => match &step.image {
            Some(image) => format!("{image}:{}", step.node_name()),
            None => step.node_name(),
        },
    };
    let ordinal = steps[..index]
        .iter()
        .filter(|prior| prior.phase == step.phase && prior.node_name() == step.node_name())
        .count();
    if ordinal == 0 {
        base
    } else {
        format!("{base}#{}", ordinal + 1)
    }
}

/// The paired stamp inside one flat PG clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PgStamp {
    /// Epoch copied verbatim from [`WorldStamp::world_epoch`].
    pub world_epoch: String,
}

impl PgStamp {
    /// The checkpoint path inside `pg_clone`.
    #[must_use]
    pub fn path(pg_clone: &Path) -> PathBuf {
        pg_clone.join(PG_STAMP_FILE)
    }

    /// Reads the checkpoint when it exists and parses.
    #[must_use]
    pub fn load(pg_clone: &Path) -> Option<Self> {
        let bytes = fs::read(Self::path(pg_clone)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// Whether a stored world stamp may be reused for this boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpochState {
    /// Both sides agree on `world_epoch`; stamped work is reusable.
    Matched(String),
    /// No world stamp exists yet.
    Missing,
    /// A stamp exists but its epoch is empty or disagrees with the PG sibling.
    Mismatched,
}

impl EpochState {
    /// Whether the stamps may be trusted.
    #[must_use]
    pub fn is_matched(&self) -> bool {
        matches!(self, Self::Matched(_))
    }
}

/// `<world>/.slipfork-world-stamp.json` reader/writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldStampStore {
    world_dir: PathBuf,
}

impl WorldStampStore {
    /// Builds the store for one world subvolume.
    #[must_use]
    pub fn new(world_dir: impl Into<PathBuf>) -> Self {
        Self {
            world_dir: world_dir.into(),
        }
    }

    /// The checkpoint path.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.world_dir.join(WORLD_STAMP_FILE)
    }

    /// Reads the checkpoint when it exists and parses.
    #[must_use]
    pub fn load(&self) -> Option<WorldStamp> {
        let bytes = fs::read(self.path()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Writes the checkpoint with an atomic tmp+rename.
    pub fn save(&self, stamp: &WorldStamp) -> Result<()> {
        write_json(&self.path(), stamp)
    }

    /// Verifies the world stamp against the paired PG sibling (when the world has one).
    #[must_use]
    pub fn verify(&self, pg_clone: Option<&Path>) -> EpochState {
        self.verify_all(pg_clone)
    }

    /// Verifies the world stamp against **every** paired clone; one stale sibling is a mismatch.
    #[must_use]
    pub fn verify_all<'a>(&self, clones: impl IntoIterator<Item = &'a Path>) -> EpochState {
        let Some(world) = self.load() else {
            return EpochState::Missing;
        };
        if world.world_epoch.is_empty() {
            return EpochState::Mismatched;
        }
        for clone in clones {
            match PgStamp::load(clone) {
                Some(pg) if pg.world_epoch == world.world_epoch => {}
                _ => return EpochState::Mismatched,
            }
        }
        EpochState::Matched(world.world_epoch)
    }

    /// The stamp a fork may reuse: `Some` only when the stored `world_epoch` and every paired
    /// clone match `(plan, features)`'s epoch, so no caller re-derives trust on its own and a
    /// cross-feature boot (`-F -dev`, `--no-default-features`) never adopts another mode's images.
    #[must_use]
    pub fn trusted_stamp<'a>(
        &self,
        plan: &WorldPlan,
        features: &FeatureSet,
        clones: impl IntoIterator<Item = &'a Path>,
    ) -> Option<WorldStamp> {
        match self.verify_all(clones) {
            EpochState::Matched(stored) if stored == plan_epoch(plan, features) => self.load(),
            _ => None,
        }
    }
}

/// Atomically writes the PG checkpoint carrying `epoch`.
pub fn write_pg_checkpoint(pg_clone: &Path, epoch: &str) -> Result<()> {
    write_json(
        &PgStamp::path(pg_clone),
        &PgStamp {
            world_epoch: epoch.to_owned(),
        },
    )
}

/// Atomically writes the PG checkpoint carrying `epoch` into every paired clone.
pub fn write_pg_checkpoints(clones: &[PathBuf], epoch: &str) -> Result<()> {
    for clone in clones {
        write_pg_checkpoint(clone, epoch)?;
    }
    Ok(())
}

/// A stable epoch for one `(plan, features)` pair, so a fork with the same `Forkfile` and active
/// feature set derives the same epoch while any feature override invalidates inherited stamps.
#[must_use]
pub fn plan_epoch(plan: &WorldPlan, features: &FeatureSet) -> String {
    let mut seed = String::from("features|");
    for (index, name) in features.active().iter().enumerate() {
        if index > 0 {
            seed.push(',');
        }
        seed.push_str(name);
    }
    seed.push('\n');
    for step in &plan.steps {
        seed.push_str(&format!(
            "{}|{}|{}|{}|{:?}\n",
            step.line,
            step.label,
            step.command.as_deref().unwrap_or(""),
            step.subject,
            step.deps
        ));
    }
    for image in &plan.images {
        seed.push_str(&format!(
            "image|{}|{}\n",
            image.id,
            image.from.as_deref().unwrap_or("")
        ));
    }
    epoch_for(&seed)
}

/// Short deterministic digest of `seed`, used as the shared `world_epoch`.
#[must_use]
pub fn epoch_for(seed: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(seed.as_bytes());
    let mut out = String::with_capacity(24);
    for byte in &digest[..12] {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| Error::io("create", parent, source))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| Error::Protocol {
        message: error.to_string(),
    })?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(|source| Error::io("write", &temporary, source))?;
    fs::rename(&temporary, path).map_err(|source| Error::io("rename", path, source))
}
