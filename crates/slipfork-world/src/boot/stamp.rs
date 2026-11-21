//! Stamp persistence: per-step digests plus the epoch-paired world/PG checkpoint stamps.

mod epoch;

pub use epoch::{
    EpochState, PG_STAMP_FILE, WORLD_STAMP_FILE, WorldStamp, WorldStampStore, epoch_for,
    plan_epoch, write_pg_checkpoint, write_pg_checkpoints,
};

use std::fs;
use std::path::{Path, PathBuf};

use slipfork_forkfile::boot::{SetupNode, stamp_of};

use crate::error::{Error, Result};

/// The stamp a conditional setup step must match to be skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepStamp {
    /// `.slipfork/<key>` file name.
    pub key: String,
    /// Content digest written after a successful run.
    pub digest: String,
}

/// Stable `.slipfork/setup-<digest>.stamp` name, hashed with the compiler's own `stamp_of`.
pub(crate) fn stamp_key(node: &SetupNode) -> String {
    let inputs = vec![
        (
            PathBuf::from("image"),
            Some(node.image.clone().into_bytes()),
        ),
        (
            PathBuf::from("directory"),
            Some(node.directory.to_string_lossy().into_owned().into_bytes()),
        ),
        (
            PathBuf::from("command"),
            Some(node.command.clone().into_bytes()),
        ),
        (
            PathBuf::from("tool"),
            node.tool.clone().map(String::into_bytes),
        ),
    ];
    format!("setup-{}.stamp", stamp_of(&inputs))
}

/// `<project>/.slipfork`: stamps live in the project subvolume and survive `fork`.
///
/// A stamp is only trusted when the world and its PG sibling carry the same `world_epoch`;
/// otherwise every conditional step re-runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampStore {
    root: PathBuf,
    trusted: bool,
}

impl StampStore {
    /// Builds a store whose stamps are honoured only when the epoch pair matches.
    #[must_use]
    pub fn gated(project: &Path, trusted: bool) -> Self {
        Self {
            root: project.join(".slipfork"),
            trusted,
        }
    }

    /// Whether the recorded stamp equals the planned digest.
    #[must_use]
    pub fn is_current(&self, stamp: &StepStamp) -> bool {
        self.trusted
            && fs::read_to_string(self.root.join(&stamp.key))
                .is_ok_and(|text| text.trim() == stamp.digest)
    }

    /// Persists the digest atomically after a successful conditional step.
    pub fn record(&self, stamp: &StepStamp) -> Result<()> {
        fs::create_dir_all(&self.root).map_err(|source| Error::io("create", &self.root, source))?;
        let target = self.root.join(&stamp.key);
        let temporary = self.root.join(format!("{}.tmp", stamp.key));
        fs::write(&temporary, format!("{}\n", stamp.digest).as_bytes())
            .map_err(|source| Error::io("write", &temporary, source))?;
        fs::rename(&temporary, &target).map_err(|source| Error::io("rename", &target, source))
    }
}
