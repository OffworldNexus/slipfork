//! Durable teardown journal: where a `Drop` reports a failure after its caller is gone.
//!
//! `WorldSupervisor::drop` and `WorldEgress::drop` cannot return errors, and their stderr may
//! already be closed. Both therefore append one `E`-framed row inside the world's durable capture
//! directory (`<logs_root>/<world>/supervisor.log`, beside `slirp4netns.log`) through the canonical
//! [`crate::pod_log`] writer, so `slipfork logs` and world-log pruning see teardown failures.

use std::path::{Path, PathBuf};

use crate::control::Stream;
use crate::layout::WorldName;

/// One world's durable log root and identity, cheap to clone into a teardown struct.
#[derive(Debug, Clone)]
pub(super) struct Journal {
    logs_root: PathBuf,
    world: WorldName,
}

impl Journal {
    /// Binds the journal to the injected store log root and one validated world name.
    pub(super) fn new(logs_root: &Path, world: &WorldName) -> Self {
        Self {
            logs_root: logs_root.to_path_buf(),
            world: world.clone(),
        }
    }

    /// `<logs_root>/<world>/slirp4netns.log`: the forwarder capture beside the pod logs.
    pub(super) fn egress_log(&self) -> PathBuf {
        super::egress::log_path(&self.logs_root, &self.world)
    }

    /// Appends one stderr-framed row to `<logs_root>/<world>/supervisor.log`.
    ///
    /// A teardown path has no second place to report an append failure, so that failure is the one
    /// this method swallows.
    pub(super) fn record(&self, message: &str) {
        let _ = crate::pod_log::append_stream(
            &self.logs_root.join(self.world.as_str()),
            "supervisor",
            Stream::Stderr,
            message.as_bytes(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The journal row lands in the world capture dir with the canonical `<epoch_ms>\tE\t<text>\n`
    /// framing, so `slipfork logs` and pruning see it; no stray `<world>.log` sibling appears.
    #[test]
    fn record_writes_one_stderr_framed_row_to_the_world_capture() {
        let root = tempfile::TempDir::new_in("/var/tmp").expect("disk-backed log root");
        let world = WorldName::parse("demo").expect("valid world name");
        let journal = Journal::new(root.path(), &world);
        journal.record("slipfork-world: teardown failed: wait: ECHILD");

        let path = root.path().join("demo/supervisor.log");
        let body = std::fs::read_to_string(&path).expect("world-scoped journal row");
        assert!(body.ends_with('\n'), "{body:?}");
        let mut fields = body.trim_end_matches('\n').splitn(3, '\t');
        assert!(
            fields.next().is_some_and(|ts| ts.parse::<u64>().is_ok()),
            "{body:?}"
        );
        assert_eq!(fields.next(), Some("E"), "{body:?}");
        assert_eq!(
            fields.next(),
            Some("slipfork-world: teardown failed: wait: ECHILD")
        );
        assert!(
            !root.path().join("demo.log").exists(),
            "the journal row escaped the world capture dir"
        );
    }

    /// The forwarder log sits beside the world's pod captures, never in the tmpfs pod root.
    #[test]
    fn egress_log_sits_beside_the_world_pod_logs() {
        let root = tempfile::TempDir::new_in("/var/tmp").expect("disk-backed log root");
        let world = WorldName::parse("demo").expect("valid world name");
        let journal = Journal::new(&root.path().join("logs"), &world);
        assert_eq!(
            journal.egress_log(),
            root.path().join("logs/demo/slirp4netns.log")
        );
    }
}
