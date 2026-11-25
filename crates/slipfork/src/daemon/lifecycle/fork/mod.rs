//! `fork` and `drop`: one CoW snapshot of project + world data, a branch switch, and one boot.
//!
//! PostgreSQL keeps a just-committed row in user-space `shared_buffers`/`wal_buffers` until a
//! checkpoint writes it into the kernel page cache, so `syncfs(2)` alone cannot see it. Before
//! `BtrfsEngine::fork` snapshots anything, the daemon dials `127.0.0.1:5432` through
//! `<pods>/control.sock` (`SCM_RIGHTS`) and speaks the PostgreSQL v3 wire protocol directly: one
//! `CHECKPOINT` in `< 5ms` with zero `crun`/`psql` spawn, then the fork's `syncfs` commits that
//! page cache. Btrfs ioctls stay inside `slipfork_world` and branch topology stays behind the
//! injected `VcsWorkspace` port.

mod drop;
mod failure;
mod features;
mod snapshot;
mod target;

use self::features::FEATURE_REBUILD_ROW;
pub(in crate::daemon) use snapshot::PreparedFork;
pub(in crate::daemon) use target::ForkTarget;

use std::path::PathBuf;

use serde_json::{Value, json};
use slipfork_world::{ForkName, WorldName};

use self::failure::first_line;
use super::core::Core;
use super::plan;
use crate::daemon::Failure;
use crate::daemon::boot::BootSession;
use crate::daemon::rpc::key_of;

impl Core {
    /// Snapshots the fork and reserves its boot in one locked step after `CHECKPOINT` finished.
    ///
    /// The daemon boots the returned session through the ordinary `up` path, so a fork is never a
    /// second boot implementation. Any failure after the snapshot deletes every fork subvolume.
    pub(in crate::daemon) fn begin_fork(
        &mut self,
        target: ForkTarget,
        raw_name: &str,
        fork: ForkName,
        preflight_ms: u64,
        emit: &mut dyn FnMut(Value),
    ) -> Result<(BootSession, PreparedFork), Failure> {
        if !self.worlds.contains_key(&key_of(&target.project)) {
            return Err(Failure::new(
                "no-world",
                "no world is running in this directory",
            ));
        }
        // Refuse a duplicate name before the first progress row: an existing checkout, world
        // snapshot, or leftover clone would otherwise fail the snapshot mid-stream with a raw
        // `File exists (os error 17)`.
        if self.fork_artifacts_exist(&target, &fork) {
            return Err(Failure::new(
                "fork-exists",
                format!(
                    "fork \"{}\" already exists\nnext: slipfork drop {} -f",
                    fork.as_str(),
                    fork.as_str()
                ),
            ));
        }
        if let Some(request) = &target.features {
            Self::validate_features(&target.project, request)?;
        }
        // One plan frame, before the snapshot mutates anything: the fork's own prep rows, an
        // explicit reuse-declined notice when feature overrides force image rebuilds, plus every
        // world step the fork will boot under its resolved feature set.
        let rows = Self::planned_fork_rows(&target.project, raw_name, target.features.as_ref());
        plan::emit(&rows, rows.len(), emit);
        let request = target.features.clone();
        let prepared = self.snapshot(target, raw_name, fork, preflight_ms, &rows, emit)?;
        if let Some(request) = request {
            if let Err(failure) = Self::apply_features(&prepared.project, request) {
                self.discard_fork(&prepared);
                return Err(failure);
            }
            emit(json!({
                "event": "up-to-date",
                "kind": FEATURE_REBUILD_ROW.0,
                "name": FEATURE_REBUILD_ROW.1,
                "state": "declined",
                "elapsed_ms": 0,
                "duration_ms": 0,
            }));
        }
        // The fork checkout exists only after `snapshot`; seal its heads (parent image nodes are
        // warm, so this is `cached`) before spawning `__supervise`. A failure rolls the fork back
        // instead of booting from a half-built head set.
        let fork_world = WorldName::parse(&prepared.world)
            .map_err(|error| Failure::new("invalid-world", first_line(&error.to_string())))?;
        let sealed = crate::daemon::reconcile::WorldBuild {
            host: &self.host,
            project: &prepared.project,
            world: &fork_world,
            rows: &rows,
        }
        .seal(false, emit);
        if let Err(failure) = sealed {
            self.discard_fork(&prepared);
            return Err(failure);
        }
        let planned = match crate::daemon::lease::prepare(
            &self.host,
            &prepared.project,
            &fork_world,
            Some(&prepared.parent_world),
        ) {
            Ok(planned) => planned,
            Err(failure) => {
                self.discard_fork(&prepared);
                return Err(failure);
            }
        };
        if let Err(error) =
            crate::daemon::lease::collect_garbage(&self.host, &prepared.project, Some(&planned.1))
        {
            eprintln!("slipfork daemon: snapshot gc: {error}");
        }
        let boot = json!({
            "project": prepared.project.clone(),
            "world": prepared.world.clone(),
        });
        let Some(root_row) = self.root_row(&prepared.parent) else {
            self.discard_fork(&prepared);
            return Err(Failure::new(
                "no-world",
                "the fork's root world has no ledger row\nnext: run `slipfork up` in the root checkout, then retry `slipfork fork`",
            ));
        };
        // The immediate parent edge comes from the snapshot's own `ForkPaths.parent`; a fork of a
        // fork keeps its real ancestry instead of flattening every child onto the root.
        let parent_id = self
            .ledger
            .list()
            .ok()
            .and_then(|rows| {
                rows.worlds
                    .into_iter()
                    .find(|row| row.world_name == prepared.parent_world.as_str())
                    .map(|row| row.id)
            })
            .unwrap_or(root_row.id);
        let fork_row = match self.ledger.begin_fork(slipfork_world::ForkRequest {
            root_id: root_row.id,
            parent_id,
            name: prepared.fork_name.clone(),
            runtime_dir: self.host.runtime_dir.clone(),
            branch: raw_name.to_owned(),
            now_ms: crate::daemon::now_ms(),
        }) {
            Ok(row) => row,
            Err(error) => {
                self.discard_fork(&prepared);
                return Err(crate::daemon::lifecycle::core::ledger_failure(error));
            }
        };
        match self.begin_up(&boot, planned, Some(fork_row.world_id)) {
            Ok(mut session) => {
                session.reserve_fork(prepared.fork.clone());
                session.plan_already_announced();
                Ok((session, prepared))
            }
            Err(failure) => {
                self.discard_fork(&prepared);
                Err(failure)
            }
        }
    }
}

fn project_param(params: &Value) -> Result<PathBuf, Failure> {
    params
        .get("project")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("bad-params", "`project` is required"))
}

fn param_name(params: &Value) -> Option<&str> {
    params.get("name").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use slipfork_forkfile::FeatureEdit;

    use super::Core;
    use crate::world::features::WorldFeatures;

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    /// A fork boots from the snapshot's inherited file; the request's edits must be persisted
    /// into the fork checkout, not merely validated in memory.
    #[test]
    fn apply_features_persists_the_merged_edits_into_the_fork_checkout() {
        let root = temp("slipfork-fork-features");
        let project = root.path().join("feat-auth");
        fs::create_dir_all(project.join(".slipfork")).expect("create the fork checkout");
        fs::write(
            project.join("Forkfile"),
            "# forkfile: 0.1\n\nfeatures {\n  dev\n  mail\n}\n",
        )
        .expect("write the fork's Forkfile");
        WorldFeatures::new(false, vec![FeatureEdit::Enable("mail".to_owned())])
            .save(&project)
            .expect("write the inherited features file");

        let request = WorldFeatures::new(false, vec![FeatureEdit::Disable("dev".to_owned())]);
        let applied = Core::apply_features(&project, request).expect("apply the fork's features");
        let persisted = WorldFeatures::load(&project).expect("read the persisted file");
        assert_eq!(persisted, applied);
        assert!(
            persisted
                .edits
                .contains(&FeatureEdit::Enable("mail".to_owned())),
            "the inherited edit survives: {persisted:?}"
        );
        assert!(
            persisted
                .edits
                .contains(&FeatureEdit::Disable("dev".to_owned()))
        );
        let text = fs::read_to_string(WorldFeatures::path(&project)).expect("features.json");
        assert!(text.contains("\"+mail\""), "{text}");
        assert!(text.contains("\"-dev\""), "{text}");

        let reset = WorldFeatures::new(true, Vec::new());
        let applied = Core::apply_features(&project, reset).expect("reset the base");
        assert!(applied.no_default);
        assert!(applied.edits.is_empty(), "{applied:?}");
        assert!(
            WorldFeatures::load(&project)
                .expect("read the reset file")
                .no_default,
            "an explicit reset is persisted too"
        );

        let checked = slipfork_forkfile::compile(
            &fs::read_to_string(project.join("Forkfile")).expect("read Forkfile"),
        )
        .expect("compile Forkfile");
        let resolved = WorldFeatures::load_resolved(&project, &checked.document)
            .expect("resolve persisted fork features");
        assert!(
            resolved.active().is_empty(),
            "persisted `--no-default-features` resolves an empty active set: {resolved:?}"
        );

        let inherited_rows = Core::planned_fork_rows(&project, "feat-auth", None);
        assert!(
            !inherited_rows
                .iter()
                .any(|(kind, _)| kind == super::FEATURE_REBUILD_ROW.0),
            "a fork without feature overrides keeps the 0ms plan rows: {inherited_rows:?}"
        );
        let override_req = WorldFeatures::new(false, vec![FeatureEdit::Disable("dev".to_owned())]);
        let override_rows = Core::planned_fork_rows(&project, "feat-auth", Some(&override_req));
        assert!(
            override_rows.contains(&(
                super::FEATURE_REBUILD_ROW.0.to_owned(),
                super::FEATURE_REBUILD_ROW.1.to_owned(),
            )),
            "a feature-overridden fork announces why images rebuild: {override_rows:?}"
        );

        fs::remove_file(project.join("Forkfile")).expect("remove Forkfile");
        let missing = Core::apply_features(&project, WorldFeatures::new(true, Vec::new()))
            .expect_err("missing Forkfile must fail closed");
        assert_eq!(missing.code, "forkfile");
        assert_eq!(
            missing.message,
            "cannot read `Forkfile`\nnext: restore the file, then retry `slipfork fork`"
        );
    }
}
