//! Pure per-service pod primitives: CoW volume plans, deterministic mount override order, and the
//! plain values the engine's shared-namespace `crun` surface consumes.
//!
//! One pod is one `crun` container joined to `slipfork-world-init`'s user, network, and IPC
//! namespaces. Everything here is computed from plain injected paths and declarations: no bind, no
//! spawn, no environment read, no global. The engine adapter owns the OCI bundle, the mounts, and
//! the `execve`; this module owns the decisions only.
//!
//! The OCI bundle holds `config.json` with the service's fully resolved environment (secrets
//! included), so it never lives inside the world subvolume: the engine binds it under
//! `<ctl>/<world_id>/oci/<service>/` (0700, outside the subvolume) and injects that path here.
//! A seal or fork therefore never captures runtime state (`ARCHITECTURE.md` §3.3).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::identity::is_valid_dns_label;
use crate::port::MountMode;

/// Directory, below the world subvolume, that owns every service's CoW volumes.
pub const VOLUMES_DIR: &str = "volumes";

/// Fail-closed pod planning errors, each naming the declaration and its one-line fix.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PodError {
    /// A service id is not a single lowercase DNS label.
    #[error(
        "service id '{id}' is not a single lowercase DNS label (`[a-z0-9-]`, no edge `-`)\n  fix: rename the block to a slug such as `service api ./api`"
    )]
    InvalidServiceId {
        /// Rejected id.
        id: String,
    },
    /// A declared `volume <rel_path>` escapes or names no directory.
    #[error(
        "`volume {rel_path}` in service '{service}' is not a relative path inside the service directory ({reason})\n  fix: declare a path such as `volume .venv` or `volume app/cache`, never an absolute path or `..`"
    )]
    InvalidVolumePath {
        /// Service owning the volume.
        service: String,
        /// Declared path.
        rel_path: String,
        /// Why it cannot name a CoW subvolume.
        reason: &'static str,
    },
    /// Two declarations would share one CoW subvolume.
    #[error(
        "service '{service}' declares `volume {first}` and `volume {second}` at the same path `{path}`\n  fix: keep one `volume` line per directory; volumes append, they never stack"
    )]
    DuplicateVolume {
        /// Service owning the volumes.
        service: String,
        /// First declaration.
        first: String,
        /// Second declaration.
        second: String,
        /// Colliding subvolume path.
        path: String,
    },
    /// A service declares no `run` command.
    #[error(
        "service '{service}' declares no run command\n  fix: add `run <argv...>` to the `service {service}` block"
    )]
    EmptyArgv {
        /// Service with no argv.
        service: String,
    },
    /// A planned path is not absolute.
    #[error(
        "pod planning needs an absolute {field} path, got '{path}'\n  fix: pass the resolved world subvolume, OCI bundle, rootfs, worktree, and service directory"
    )]
    NotAbsolute {
        /// Which input was relative.
        field: &'static str,
        /// Rejected path.
        path: String,
    },
    /// The OCI bundle was placed inside the world subvolume.
    #[error(
        "OCI bundle '{bundle}' is inside world subvolume '{world_subvol}'; its `config.json` carries resolved secrets and must never be sealed or forked\n  fix: put bundles under `<ctl>/<world_id>/oci/<service>/`, outside the subvolume"
    )]
    BundleInsideSubvolume {
        /// Rejected bundle directory.
        bundle: String,
        /// World subvolume it is inside.
        world_subvol: String,
    },
}

/// One `volume <rel_path>` backed by its own Btrfs subvolume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodVolume {
    /// Declared path relative to the service directory, normalized.
    pub rel_path: PathBuf,
    /// Backing subvolume: `<world_subvol>/volumes/<service>/<sanitized_rel>`.
    pub host_subvolume: PathBuf,
    /// Mount target inside the container: `<service_dir>/<rel_path>`.
    pub target: PathBuf,
}

/// Planned CoW volumes for one service, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumePlan {
    service: String,
    volumes: Vec<PodVolume>,
}

impl VolumePlan {
    /// Plan every `volume <rel_path>` of one service.
    ///
    /// `service_dir` is the absolute in-container directory the service's `./dir` maps to
    /// (`<worktree>/<dir>`); each target is `service_dir/<rel_path>`. Sanitization is
    /// component-wise: bytes outside `[A-Za-z0-9._-]` become `_`, so a declared path can never
    /// reach outside `<world_subvol>/volumes/<service>/`. Two declarations that would land on the
    /// same subvolume fail closed instead of silently sharing state.
    pub fn new(
        world_subvol: &Path,
        service: &str,
        service_dir: &Path,
        rel_paths: &[String],
    ) -> Result<Self, PodError> {
        require_absolute("world subvolume", world_subvol)?;
        require_absolute("service directory", service_dir)?;
        if !is_valid_dns_label(service) {
            return Err(PodError::InvalidServiceId {
                id: service.to_owned(),
            });
        }
        let root = world_subvol.join(VOLUMES_DIR).join(service);
        let mut volumes = Vec::with_capacity(rel_paths.len());
        let mut planned: BTreeMap<PathBuf, &str> = BTreeMap::new();
        for declared in rel_paths {
            let (rel_path, sanitized) = plan_relative(service, declared)?;
            let host_subvolume = root.join(&sanitized);
            if let Some(first) = planned.insert(host_subvolume.clone(), declared) {
                return Err(PodError::DuplicateVolume {
                    service: service.to_owned(),
                    first: first.to_owned(),
                    second: declared.clone(),
                    path: host_subvolume.display().to_string(),
                });
            }
            let target = service_dir.join(&rel_path);
            volumes.push(PodVolume {
                rel_path,
                host_subvolume,
                target,
            });
        }
        Ok(Self {
            service: service.to_owned(),
            volumes,
        })
    }

    /// Service the plan belongs to.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Planned volumes, in declaration order.
    pub fn volumes(&self) -> &[PodVolume] {
        &self.volumes
    }

    /// Bind the live host worktree first, then every volume subvolume over its target.
    ///
    /// Order is the override semantics: later binds land on top, so the live worktree covers the
    /// whole service directory (`inotify` hot reload intact) and each CoW volume re-exposes only
    /// its own subtree (`~20 ms` fork-safe `.venv` / `node_modules` state).
    pub fn override_mounts(&self, worktree: &Path) -> Result<Vec<PodMount>, PodError> {
        require_absolute("worktree", worktree)?;
        let mut mounts = Vec::with_capacity(1 + self.volumes.len());
        mounts.push(PodMount {
            source: worktree.to_path_buf(),
            target: worktree.to_path_buf(),
            mode: MountMode::ReadWrite,
        });
        mounts.extend(self.volumes.iter().map(|volume| PodMount {
            source: volume.host_subvolume.clone(),
            target: volume.target.clone(),
            mode: MountMode::ReadWrite,
        }));
        Ok(mounts)
    }
}

/// One bind mount applied before the service's `execve`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodMount {
    /// World path bound from.
    pub source: PathBuf,
    /// Container path bound over.
    pub target: PathBuf,
    /// Read-only or read-write.
    pub mode: MountMode,
}

/// The three namespaces every service pod joins from `slipfork-world-init`.
///
/// The paths are injected by the engine (the reference engine passes
/// `/proc/<world-init>/ns/{user,net,ipc}`); this type owns only the logical join names and the
/// absolute paths it was given, never a `/proc` probe of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodNamespaces {
    /// User namespace join (`/proc/<world-init>/ns/user`).
    pub userns: PathBuf,
    /// Network namespace join (`/proc/<world-init>/ns/net`); services listen on
    /// `127.0.0.1:<port>` here.
    pub netns: PathBuf,
    /// IPC namespace join (`/proc/<world-init>/ns/ipc`).
    pub ipcns: PathBuf,
}

impl PodNamespaces {
    /// The injected join paths of one live `slipfork-world-init` process.
    pub fn new(userns: &Path, netns: &Path, ipcns: &Path) -> Result<Self, PodError> {
        require_absolute("user namespace join", userns)?;
        require_absolute("network namespace join", netns)?;
        require_absolute("IPC namespace join", ipcns)?;
        Ok(Self {
            userns: userns.to_path_buf(),
            netns: netns.to_path_buf(),
            ipcns: ipcns.to_path_buf(),
        })
    }

    /// `(namespace type, join path)` pairs the OCI bundle writer emits, in `user`/`network`/`ipc`
    /// order.
    pub fn entries(&self) -> [(&'static str, &Path); 3] {
        [
            ("user", self.userns.as_path()),
            ("network", self.netns.as_path()),
            ("ipc", self.ipcns.as_path()),
        ]
    }
}

/// One service's direct `execve` command and its complete container environment.
///
/// [`fmt::Debug`] never renders argv slots or environment values: both may carry resolved
/// secrets, so diagnostics print the argument count and variable names only.
#[derive(Clone, PartialEq, Eq)]
pub struct PodCommand {
    /// Working directory inside the container (`<worktree>/<service_dir>`).
    pub cwd: PathBuf,
    /// Direct argv — never a `/bin/sh -c` wrapper.
    pub argv: Vec<String>,
    /// Complete environment of the container process.
    pub env: BTreeMap<String, String>,
}

impl PodCommand {
    /// A direct command: working directory, argv, and complete environment.
    pub fn new(cwd: PathBuf, argv: Vec<String>, env: BTreeMap<String, String>) -> Self {
        Self { cwd, argv, env }
    }
}

impl fmt::Debug for PodCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PodCommand")
            .field("cwd", &self.cwd)
            .field("argv_len", &self.argv.len())
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Injected absolute locations of one pod.
///
/// The OCI bundle is owned by the engine's control directory (`<ctl>/<world_id>/oci/<service>/`,
/// `0700`), never the world subvolume; [`PodLocation::new`] enforces that boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodLocation {
    /// The world's one Btrfs subvolume.
    pub world_subvol: PathBuf,
    /// OCI bundle directory outside that subvolume.
    pub bundle_dir: PathBuf,
    /// Read-only container root filesystem.
    pub rootfs: PathBuf,
}

impl PodLocation {
    /// Validate the three injected absolute locations.
    pub fn new(world_subvol: &Path, bundle_dir: &Path, rootfs: &Path) -> Result<Self, PodError> {
        require_absolute("world subvolume", world_subvol)?;
        require_absolute("OCI bundle directory", bundle_dir)?;
        require_absolute("container rootfs", rootfs)?;
        if bundle_dir.starts_with(world_subvol) {
            return Err(PodError::BundleInsideSubvolume {
                bundle: bundle_dir.display().to_string(),
                world_subvol: world_subvol.display().to_string(),
            });
        }
        Ok(Self {
            world_subvol: world_subvol.to_path_buf(),
            bundle_dir: bundle_dir.to_path_buf(),
            rootfs: rootfs.to_path_buf(),
        })
    }
}

/// Everything one `crun` container needs, computed without touching the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePodSpec {
    /// Stable service id (`api`, `mailpit`), derived from the volume plan.
    pub service: String,
    /// Injected OCI bundle directory (control directory, outside the world subvolume).
    pub bundle_dir: PathBuf,
    /// Bundled read-only container root filesystem.
    pub rootfs: PathBuf,
    /// Direct command and complete environment.
    pub command: PodCommand,
    /// CoW volumes, in declaration order.
    pub volumes: VolumePlan,
    /// Namespaces joined from the world init process.
    pub namespaces: PodNamespaces,
}

impl ServicePodSpec {
    /// Plan one service pod from its injected locations.
    pub fn new(
        location: PodLocation,
        namespaces: PodNamespaces,
        volumes: VolumePlan,
        command: PodCommand,
    ) -> Result<Self, PodError> {
        require_absolute("service working directory", &command.cwd)?;
        if command.argv.is_empty() {
            return Err(PodError::EmptyArgv {
                service: volumes.service().to_owned(),
            });
        }
        Ok(Self {
            service: volumes.service().to_owned(),
            bundle_dir: location.bundle_dir,
            rootfs: location.rootfs,
            command,
            volumes,
            namespaces,
        })
    }

    /// Working directory inside the container (`process.cwd` of the OCI bundle).
    pub fn cwd(&self) -> &Path {
        &self.command.cwd
    }

    /// Direct argv of the service process.
    pub fn argv(&self) -> &[String] {
        &self.command.argv
    }

    /// `(namespace type, join path)` pairs for the OCI bundle's `linux.namespaces[].path`.
    pub fn namespace_joins(&self) -> [(&'static str, &Path); 3] {
        self.namespaces.entries()
    }

    /// `MS_BIND` the live worktree first, then every volume subvolume over its target.
    pub fn mounts(&self, worktree: &Path) -> Result<Vec<PodMount>, PodError> {
        self.volumes.override_mounts(worktree)
    }
}

impl fmt::Display for ServicePodSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.service, self.cwd().display())
    }
}

fn require_absolute(field: &'static str, path: &Path) -> Result<(), PodError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(PodError::NotAbsolute {
            field,
            path: path.display().to_string(),
        })
    }
}

/// Normalize `declared` and sanitize each `/`-separated component into a subvolume-relative path.
fn plan_relative(service: &str, declared: &str) -> Result<(PathBuf, PathBuf), PodError> {
    let invalid = |reason: &'static str| PodError::InvalidVolumePath {
        service: service.to_owned(),
        rel_path: declared.to_owned(),
        reason,
    };
    if declared.trim().is_empty() {
        return Err(invalid("an empty path names no directory"));
    }
    if declared.starts_with('/') {
        return Err(invalid("an absolute path escapes the service"));
    }
    let mut rel_path = PathBuf::new();
    let mut sanitized = PathBuf::new();
    for component in declared.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(invalid("`..` would escape the service directory")),
            name => {
                rel_path.push(name);
                sanitized.push(sanitize_component(name));
            }
        }
    }
    if rel_path.as_os_str().is_empty() {
        return Err(invalid("the path names no directory"));
    }
    Ok((rel_path, sanitized))
}

/// ASCII-anchored component sanitization: anything outside `[A-Za-z0-9._-]` becomes `_`.
fn sanitize_component(component: &str) -> String {
    component
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> PathBuf {
        PathBuf::from("/var/tmp/slipfork/worlds/acme")
    }

    fn rootfs() -> PathBuf {
        PathBuf::from("/var/tmp/slipfork/cache/rootfs/abc/app")
    }

    fn bundle() -> PathBuf {
        PathBuf::from("/var/tmp/slipfork/ctl/acme/oci/api")
    }

    fn worktree() -> PathBuf {
        PathBuf::from("/home/dev/acme")
    }

    fn service_dir() -> PathBuf {
        worktree().join("api")
    }

    fn rels(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    fn plan(paths: &[&str]) -> VolumePlan {
        VolumePlan::new(&world(), "api", &service_dir(), &rels(paths)).expect("valid volume plan")
    }

    fn namespaces() -> PodNamespaces {
        PodNamespaces::new(
            Path::new("/proc/4242/ns/user"),
            Path::new("/proc/4242/ns/net"),
            Path::new("/proc/4242/ns/ipc"),
        )
        .expect("injected absolute joins")
    }

    fn command() -> PodCommand {
        PodCommand::new(
            service_dir(),
            vec!["uv".to_owned(), "run".to_owned(), "gunicorn".to_owned()],
            BTreeMap::from([("DATABASE_URL".to_owned(), "postgres://secret".to_owned())]),
        )
    }

    fn pod(volumes: VolumePlan) -> ServicePodSpec {
        ServicePodSpec::new(
            PodLocation::new(&world(), &bundle(), &rootfs()).expect("valid locations"),
            namespaces(),
            volumes,
            command(),
        )
        .expect("valid pod")
    }

    #[test]
    fn volumes_sanitize_into_service_scoped_subvolumes_in_declaration_order() {
        let plan = plan(&[".venv", "node_modules", "app/cache", "weird name!"]);

        let hosts: Vec<&Path> = plan
            .volumes()
            .iter()
            .map(|volume| volume.host_subvolume.as_path())
            .collect();
        assert_eq!(
            hosts,
            [
                world().join("volumes/api/.venv").as_path(),
                world().join("volumes/api/node_modules").as_path(),
                world().join("volumes/api/app/cache").as_path(),
                world().join("volumes/api/weird_name_").as_path(),
            ]
        );
        assert_eq!(
            plan.volumes()[2].target,
            service_dir().join("app/cache"),
            "guest target keeps the declared path"
        );
        assert_eq!(plan.service(), "api");
    }

    #[test]
    fn live_worktree_binds_first_then_each_volume_overrides() {
        let plan = plan(&[".venv", "node_modules"]);
        let mounts = plan
            .override_mounts(&worktree())
            .expect("absolute worktree");

        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[0].source, worktree());
        assert_eq!(mounts[0].target, worktree());
        assert_eq!(mounts[0].mode, MountMode::ReadWrite);
        assert_eq!(mounts[1].target, service_dir().join(".venv"));
        assert_eq!(mounts[2].target, service_dir().join("node_modules"));
        assert_eq!(
            mounts[1].source,
            world().join("volumes/api/.venv"),
            "each volume binds from its own CoW subvolume"
        );
    }

    #[test]
    fn traversal_absolute_empty_and_relative_base_paths_fail_closed() {
        for bad in ["", " ", ".", "..", "../escape", "app/../../escape", "/abs"] {
            assert!(
                matches!(
                    VolumePlan::new(&world(), "api", &service_dir(), &rels(&[bad])),
                    Err(PodError::InvalidVolumePath { .. })
                ),
                "volume `{bad}` must fail closed"
            );
        }
        assert!(matches!(
            VolumePlan::new(Path::new("relative/world"), "api", &service_dir(), &[]),
            Err(PodError::NotAbsolute {
                field: "world subvolume",
                ..
            })
        ));
        assert!(matches!(
            VolumePlan::new(&world(), "API", &service_dir(), &[]),
            Err(PodError::InvalidServiceId { .. })
        ));
    }

    #[test]
    fn colliding_or_repeated_volume_declarations_fail_closed() {
        for paths in [
            vec![".venv", "./.venv"],
            vec!["app cache", "app_cache"],
            vec!["node modules", "node_modules"],
        ] {
            match VolumePlan::new(&world(), "api", &service_dir(), &rels(&paths)) {
                Err(PodError::DuplicateVolume { path, .. }) => {
                    assert!(
                        path.starts_with("/var/tmp/slipfork/worlds/acme/volumes/api/"),
                        "duplicate names the colliding subvolume: {path}"
                    );
                }
                other => panic!("{paths:?} must fail closed, got {other:?}"),
            }
        }
    }

    #[test]
    fn injected_namespace_joins_keep_the_logical_user_net_ipc_names() {
        let namespaces = namespaces();
        assert_eq!(namespaces.userns, PathBuf::from("/proc/4242/ns/user"));
        assert_eq!(
            namespaces.entries(),
            [
                ("user", Path::new("/proc/4242/ns/user")),
                ("network", Path::new("/proc/4242/ns/net")),
                ("ipc", Path::new("/proc/4242/ns/ipc")),
            ]
        );
        assert!(matches!(
            PodNamespaces::new(
                Path::new("relative/ns/user"),
                Path::new("/proc/1/ns/net"),
                Path::new("/proc/1/ns/ipc"),
            ),
            Err(PodError::NotAbsolute {
                field: "user namespace join",
                ..
            })
        ));
        assert_eq!(
            pod(plan(&[".venv"])).namespace_joins(),
            namespaces.entries()
        );
    }

    #[test]
    fn bundle_directories_must_stay_outside_the_world_subvolume() {
        assert_eq!(pod(plan(&[".venv"])).bundle_dir, bundle());
        assert_eq!(pod(plan(&[])).rootfs, rootfs());

        let inside = world().join("pods/api");
        assert!(matches!(
            PodLocation::new(&world(), &inside, &rootfs()),
            Err(PodError::BundleInsideSubvolume { .. })
        ));
        assert!(matches!(
            PodLocation::new(&world(), Path::new("relative/bundle"), &rootfs()),
            Err(PodError::NotAbsolute { .. })
        ));
    }

    #[test]
    fn service_id_is_derived_from_the_volume_plan() {
        let worker = VolumePlan::new(&world(), "worker", &service_dir(), &[]).expect("valid plan");
        let pod = ServicePodSpec::new(
            PodLocation::new(&world(), &bundle(), &rootfs()).expect("valid locations"),
            namespaces(),
            worker,
            command(),
        )
        .expect("valid pod");
        assert_eq!(pod.service, "worker");
        assert_eq!(pod.cwd(), service_dir());
        assert_eq!(pod.argv()[0], "uv");

        let empty = PodCommand::new(service_dir(), Vec::new(), BTreeMap::new());
        assert!(matches!(
            ServicePodSpec::new(
                PodLocation::new(&world(), &bundle(), &rootfs()).expect("valid locations"),
                namespaces(),
                plan(&[]),
                empty,
            ),
            Err(PodError::EmptyArgv { service }) if service == "api"
        ));
    }

    #[test]
    fn debug_output_exposes_environment_names_but_never_values() {
        let rendered = format!("{:?}", pod(plan(&[".venv"])));
        assert!(rendered.contains("DATABASE_URL"));
        assert!(
            !rendered.contains("postgres://secret"),
            "resolved secret values must never render: {rendered}"
        );
        assert!(rendered.contains("argv_len"));
    }
}
