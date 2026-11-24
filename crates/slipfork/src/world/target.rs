//! Resolution of the optional positional `[target]` on `up`, `down`, and `restart`.
//!
//! One canonical owner turns `(cwd, target)` plus the daemon's typed `ps` reply into a concrete
//! project, world, and (for `restart`) service. Nothing downstream re-scans paths or world ids.

use std::path::{Path, PathBuf};

use slipfork_world::WorldName;

use crate::ps::{PsReply, PsWorld};

/// One resolved verb target: the project to act on and the world it records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Project directory the daemon keys its world registry on.
    pub project: PathBuf,
    /// World id the project records, derives, or is registered under.
    pub world: WorldName,
    /// Service to restart; `None` for a world-wide verb.
    pub service: Option<String>,
}

/// A target that cannot be resolved, with its one-line message and exact remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetError {
    /// The one line a human reads before `next:`.
    pub message: String,
    /// The one corrective command.
    pub next: &'static str,
}

impl TargetError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            next: "run `slipfork ps`",
        }
    }
}

/// The resolver over this invocation's working directory and the daemon's `ps` reply.
#[derive(Debug, Clone, Copy)]
pub struct Targets<'a> {
    cwd: &'a Path,
    reply: &'a PsReply,
}

impl<'a> Targets<'a> {
    /// Builds a resolver from the injected working directory and typed `ps` reply.
    #[must_use]
    pub fn new(cwd: &'a Path, reply: &'a PsReply) -> Self {
        Self { cwd, reply }
    }

    /// Resolves a target that lives entirely under this directory: `None` means `cwd`, and
    /// `<cwd>/.forks/<target>` is read straight from disk. Returns `None` when the daemon's
    /// registry is required to resolve a non-local world or fork name.
    #[must_use]
    pub fn resolve_local(&self, target: Option<&str>) -> Option<Result<Resolved, TargetError>> {
        match target {
            None => Some(self.world_for(self.cwd).map(|world| Resolved {
                project: self.cwd.to_path_buf(),
                world,
                service: None,
            })),
            Some(target) if is_plain_name(target) => {
                let fork = self.cwd.join(slipfork_world::FORKS_DIR).join(target);
                if !fork.is_dir() {
                    return None;
                }
                Some(self.world_for(&fork).map(|world| Resolved {
                    project: fork,
                    world,
                    service: None,
                }))
            }
            Some(_) => None,
        }
    }

    /// Resolves a world or fork target; `None` means this directory's own world.
    ///
    /// # Errors
    ///
    /// Returns the exact one-line "no world or fork named ..." refusal when the target is neither
    /// a `<cwd>/.forks/<target>` checkout nor a uniquely registered world/fork id.
    pub fn resolve(&self, target: Option<&str>) -> Result<Resolved, TargetError> {
        if let Some(local) = self.resolve_local(target) {
            return local;
        }
        let target = target.expect("a local target always resolves above");
        match find(self.reply, target).as_slice() {
            [] => Err(TargetError::new(format!(
                "no world or fork named `{target}` here"
            ))),
            [world] => Ok(Resolved {
                project: world.project.clone(),
                world: WorldName::parse(&world.world)
                    .map_err(|error| TargetError::new(error.to_string()))?,
                service: None,
            }),
            worlds => {
                let candidates: Vec<String> = worlds
                    .iter()
                    .map(|world| world.project.display().to_string())
                    .collect();
                Err(TargetError::new(format!(
                    "`{target}` is ambiguous: {}",
                    candidates.join(", ")
                )))
            }
        }
    }

    /// Resolves a `restart` target: an explicit `-s` always names a service, and a bare target
    /// first matches a declared/live service of this directory's own live world.
    ///
    /// # Errors
    ///
    /// Propagates the same world/fork refusal as [`Self::resolve`].
    pub fn resolve_restart(
        &self,
        target: Option<&str>,
        service: Option<&str>,
    ) -> Result<Resolved, TargetError> {
        if service.is_none()
            && let Some(target) = target
            && let Some(world) = self.live_world_here()
            && let Some(id) = live_service(world, target)
        {
            return Ok(Resolved {
                project: self.cwd.to_path_buf(),
                world: WorldName::parse(&world.world)
                    .map_err(|error| TargetError::new(error.to_string()))?,
                service: Some(id),
            });
        }
        let mut resolved = self.resolve(target)?;
        resolved.service = service.map(str::to_owned);
        Ok(resolved)
    }

    /// The world this directory records, derives from its `.forks` layout, or slugs to.
    fn world_for(&self, project: &Path) -> Result<WorldName, TargetError> {
        super::world_name(project, None).map_err(TargetError::new)
    }

    /// The registered world or fork whose project is exactly `cwd`.
    fn live_world_here(&self) -> Option<&'a PsWorld> {
        find_project(&self.reply.worlds, self.cwd)
    }
}

/// A target usable as one `.forks` directory entry: no separators, `.`, or `..`.
fn is_plain_name(target: &str) -> bool {
    !target.is_empty()
        && target != "."
        && target != ".."
        && !target.contains('/')
        && !target.contains('\\')
}

/// Depth-first search for every world id or fork name match.
fn find<'a>(reply: &'a PsReply, target: &str) -> Vec<&'a PsWorld> {
    fn walk<'a>(worlds: &'a [PsWorld], target: &str, out: &mut Vec<&'a PsWorld>) {
        for world in worlds {
            if world.world == target || world.fork.as_deref() == Some(target) {
                out.push(world);
            }
            walk(&world.forks, target, out);
        }
    }
    let mut matches = Vec::new();
    walk(&reply.worlds, target, &mut matches);
    matches
}

/// Depth-first search for the world whose registered project is exactly `cwd`.
fn find_project<'a>(worlds: &'a [PsWorld], cwd: &Path) -> Option<&'a PsWorld> {
    for world in worlds {
        if world.project == cwd {
            return Some(world);
        }
        if let Some(found) = find_project(&world.forks, cwd) {
            return Some(found);
        }
    }
    None
}

/// The declared or live service id matching `target`, if any.
fn live_service(world: &PsWorld, target: &str) -> Option<String> {
    world
        .services
        .iter()
        .find(|service| service.id == target)
        .map(|service| service.id.clone())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::ps::PsService;

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    fn service(id: &str) -> PsService {
        PsService {
            id: id.to_owned(),
            kind: crate::ps::PsKind::Service,
            running: true,
            port: None,
            version: None,
            cdn: None,
            policy: None,
            pid: None,
            routes: Vec::new(),
            orphaned: false,
            image: None,
            command: None,
        }
    }

    fn world(project: &Path, world: &str, fork: Option<&str>) -> PsWorld {
        PsWorld {
            project: project.to_path_buf(),
            path: project.to_path_buf(),
            world: world.to_owned(),
            fork: fork.map(str::to_owned),
            url: format!("http://{world}.localhost"),
            services: vec![service("api"), service("worker")],
            ..PsWorld::default()
        }
    }

    #[test]
    fn a_missing_target_is_this_directorys_recorded_or_derived_world() {
        let root = temp("slipfork-target-cwd");
        let demo = root.path().join("demo");
        fs::create_dir_all(demo.join(".slipfork")).expect("create demo");
        fs::write(
            demo.join(".slipfork/world.json"),
            format!(
                "{{\"world\":\"feat-auth--demo\",\"socket\":\"/run/s\",\"project\":\"{}\",\"pid\":1}}",
                demo.display()
            ),
        )
        .expect("write world record");
        let reply = PsReply::default();
        let resolved = Targets::new(&demo, &reply)
            .resolve(None)
            .expect("a recorded world resolves without a daemon row");
        assert_eq!(resolved.project, demo);
        assert_eq!(resolved.world.as_str(), "feat-auth--demo");
        assert_eq!(resolved.service, None);
    }

    #[test]
    fn a_forks_checkout_resolves_its_canonical_world_id() {
        let root = temp("slipfork-target-forkdir");
        let parent = root.path().join("demo");
        let fork = parent.join(".forks").join("feat-auth");
        fs::create_dir_all(&fork).expect("create the fork checkout");
        let reply = PsReply::default();
        let resolved = Targets::new(&parent, &reply)
            .resolve(Some("feat-auth"))
            .expect("a .forks checkout resolves without a daemon row");
        assert_eq!(resolved.project, fork);
        assert_eq!(resolved.world.as_str(), "feat-auth--demo");
        assert_eq!(resolved.service, None);
    }

    #[test]
    fn a_registered_world_or_nested_fork_name_resolves_its_project() {
        let root = temp("slipfork-target-registry");
        let demo = root.path().join("demo");
        let fork = demo.join(".forks").join("feat-auth");
        let mut reply = PsReply::default();
        let mut parent = world(&demo, "demo", None);
        parent
            .forks
            .push(world(&fork, "feat-auth--demo", Some("feat-auth")));
        reply.worlds.push(parent);
        let targets = Targets::new(root.path(), &reply);
        let by_world = targets.resolve(Some("demo")).expect("registered world");
        assert_eq!(by_world.project, demo);
        assert_eq!(by_world.world.as_str(), "demo");
        let by_fork = targets
            .resolve(Some("feat-auth"))
            .expect("nested fork name");
        assert_eq!(by_fork.project, fork);
        assert_eq!(by_fork.world.as_str(), "feat-auth--demo");
    }

    #[test]
    fn restart_prefers_a_live_service_then_an_explicit_service() {
        let root = temp("slipfork-target-restart");
        let demo = root.path().join("demo");
        let fork = root.path().join("other");
        let mut reply = PsReply::default();
        reply.worlds.push(world(&demo, "demo", None));
        reply.worlds.push(world(&fork, "other", None));
        let targets = Targets::new(&demo, &reply);
        let inferred = targets
            .resolve_restart(Some("api"), None)
            .expect("a service id of the live world resolves");
        assert_eq!(inferred.project, demo);
        assert_eq!(inferred.service.as_deref(), Some("api"));
        let explicit = targets
            .resolve_restart(Some("other"), Some("worker"))
            .expect("an explicit service target still resolves the world");
        assert_eq!(explicit.project, fork);
        assert_eq!(explicit.service.as_deref(), Some("worker"));
        assert_eq!(explicit.world.as_str(), "other");
    }

    #[test]
    fn two_same_named_forks_are_ambiguous_and_name_both_projects() {
        let root = temp("slipfork-target-ambiguous");
        let first = root.path().join("alpha");
        let second = root.path().join("beta");
        let mut reply = PsReply::default();
        for parent in [&first, &second] {
            let name = parent.file_name().unwrap().to_str().unwrap();
            let mut parent_row = world(parent, name, None);
            parent_row.forks.push(world(
                &parent.join(slipfork_world::FORKS_DIR).join("feat-auth"),
                &format!("feat-auth--{name}"),
                Some("feat-auth"),
            ));
            reply.worlds.push(parent_row);
        }
        let error = Targets::new(root.path(), &reply)
            .resolve(Some("feat-auth"))
            .expect_err("two same-named forks must be refused");
        assert!(error.message.contains("is ambiguous"), "{}", error.message);
        assert!(
            error
                .message
                .contains(&first.join(".forks/feat-auth").display().to_string()),
            "{}",
            error.message
        );
        assert!(
            error
                .message
                .contains(&second.join(".forks/feat-auth").display().to_string()),
            "{}",
            error.message
        );
        assert_eq!(error.next, "run `slipfork ps`");
    }

    #[test]
    fn an_unknown_target_names_the_missing_world_and_the_one_remedy() {
        let root = temp("slipfork-target-missing");
        let reply = PsReply::default();
        let error = Targets::new(root.path(), &reply)
            .resolve(Some("ghost"))
            .expect_err("an unknown target must fail closed");
        assert_eq!(error.message, "no world or fork named `ghost` here");
        assert_eq!(error.next, "run `slipfork ps`");
        assert!(
            Targets::new(root.path(), &reply)
                .resolve(Some("../escape"))
                .is_err(),
            "a path-escaping target must never resolve a directory"
        );
    }
}
