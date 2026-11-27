//! `<key>.json` node records: sealed nodes and record-only nodes both carry one.

use std::fs;

use serde::{Deserialize, Serialize};

use super::SnapshotStore;
use crate::snapshot::{NodeKey, NodeKind, SnapshotError, SnapshotResult};

/// Version of the `<key>.json` node record.
pub const RECORD_VERSION: u32 = 1;
/// Opaque materializer facts stored beside a node.
pub type Facts = serde_json::Value;

/// `<key>.json`: the durable facts of one node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    /// Record version.
    pub v: u32,
    /// Node kind.
    pub kind: NodeKind,
    /// Parents in declared order.
    pub parents: Vec<NodeKey>,
    /// Canonical text.
    pub canon: String,
    /// Source line.
    pub line: u32,
    /// First creation time (ms since the epoch, caller-supplied).
    pub created_ms: u64,
    /// Last reuse time (ms), the gc grace clock.
    pub last_used_ms: u64,
    /// Opaque materializer facts.
    pub facts: Facts,
}

impl SnapshotStore<'_> {
    pub(crate) fn read_record(&self, key: &NodeKey) -> SnapshotResult<Option<NodeRecord>> {
        let path = self.record_path(key);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).map_err(|source| SnapshotError::Record {
            path: path.clone(),
            message: source.to_string(),
        })?;
        let record = serde_json::from_slice(&bytes).map_err(|source| SnapshotError::Record {
            path,
            message: source.to_string(),
        })?;
        Ok(Some(record))
    }

    pub(crate) fn write_record(&self, key: &NodeKey, record: &NodeRecord) -> SnapshotResult<()> {
        let path = self.record_path(key);
        let tmp = self.root.join(format!(
            "{}.json.tmp-{}-{}",
            key.as_hex(),
            std::process::id(),
            self.next_ordinal()
        ));
        let bytes = serde_json::to_vec(record).map_err(|source| SnapshotError::Record {
            path: path.clone(),
            message: source.to_string(),
        })?;
        fs::write(&tmp, bytes).map_err(|source| SnapshotError::io("write", &tmp, source))?;
        fs::rename(&tmp, &path).map_err(|source| SnapshotError::io("publish", &path, source))
    }
}
