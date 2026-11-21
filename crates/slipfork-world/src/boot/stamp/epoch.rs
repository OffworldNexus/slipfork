//! `world_epoch` checkpoints: `<world>/.slipfork-world-stamp.json` plus its paired PG sibling.
//!
//! A stamp is only reused when the world subvolume and every flat PG clone it was written with
//! carry the same epoch. `slipfork fork` snapshots both, so a cloned world skips every rebuild,
//! `@<tool>` install, and hook; a mismatched pair forces the hooks to run again.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::boot::{HookPhase, WorldPlan};
use crate::error::{Error, Result};

/// The world checkpoint written at `<world>/.slipfork-world-stamp.json`.
pub const WORLD_STAMP_FILE: &str = ".slipfork-world-stamp.json";
/// The PG sibling checkpoint written at `worlds/<world>.pg.<pod>/.slipfork-pg-stamp.json`.
pub const PG_STAMP_FILE: &str = ".slipfork-pg-stamp.json";

/// Resolved world state plus the epoch shared with every resource clone at `CHECKPOINT`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldStamp {
    /// Epoch shared with every paired PG stamp.
    #[serde(default)]
    pub world_epoch: String,
    /// Resolved image rootfs subvolume path per image id.
    #[serde(default)]
    pub images: BTreeMap<String, String>,
    /// Completed `@<tool>` / `when changed(...)` digests.
    #[serde(default)]
    pub tools: BTreeMap<String, String>,
    /// `before-start` input digests.
    #[serde(default)]
    pub before_start: BTreeMap<String, String>,
    /// `pre-deploy` input digests.
    #[serde(default)]
    pub pre_deploy: BTreeMap<String, String>,
}

impl WorldStamp {
    /// Builds the checkpoint for a completed boot: epoch, resolved images, and step digests.
    #[must_use]
    pub fn checkpoint(plan: &WorldPlan, epoch: String, images: BTreeMap<String, String>) -> Self {
        let mut stamp = Self {
            world_epoch: epoch,
            images,
            ..Self::default()
        };
        for step in &plan.steps {
            let Some(conditional) = &step.stamp else {
                continue;
            };
            match step.phase {
                Some(HookPhase::PreDeploy) => {
                    stamp
                        .pre_deploy
                        .insert(step.label.clone(), conditional.digest.clone());
                }
                Some(HookPhase::BeforeStart) => {
                    stamp
                        .before_start
                        .insert(step.label.clone(), conditional.digest.clone());
                }
                Some(HookPhase::PostDeploy | HookPhase::FailedDeploy) => {}
                None => {
                    stamp
                        .tools
                        .insert(step.label.clone(), conditional.digest.clone());
                }
            }
        }
        stamp
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

/// A stable epoch for one plan shape, so a fork of the same `Forkfile` derives the same epoch.
#[must_use]
pub fn plan_epoch(plan: &WorldPlan) -> String {
    let mut seed = String::new();
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
