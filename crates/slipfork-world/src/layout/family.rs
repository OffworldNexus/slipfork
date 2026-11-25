//! Fork family discovery: flat `<family_root>/.forks/<name>` checkouts at any depth.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::{FORKS_DIR, StoreLayout, WorldName, project_forks_dir};

/// One `<project>/.forks/<name>` checkout on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkDir {
    /// Fork name, the `.forks/<name>` directory entry.
    pub name: String,
    /// Absolute checkout path.
    pub path: PathBuf,
}

/// The `<family_root>/.forks/<innermost>` pair owning `project`, when `project` is a checkout.
///
/// Forks always land flat at `<family_root>/.forks/<name>`, so a legacy tree may still hold
/// nested `.forks` levels (`<root>/.forks/a/.forks/b`). The walk climbs every `.forks` level and
/// returns the **outermost** containing project plus the checkout's own name. This is the one
/// structural predicate every caller shares: no caller re-inspects `path.parent()` names.
#[must_use]
pub fn fork_checkout(project: &Path) -> Option<(PathBuf, super::ForkName)> {
    let fork = super::ForkName::parse(project.file_name()?.to_str()?).ok()?;
    let mut current = project.to_path_buf();
    loop {
        let forks_dir = current.parent()?;
        if forks_dir.file_name()? != OsStr::new(FORKS_DIR) {
            return None;
        }
        let parent = forks_dir.parent()?;
        // `parent` is another fork checkout exactly when its own parent is `.forks`, in which
        // case the family root is higher still. A root project named like a fork never matches,
        // because its parent is not `.forks`.
        let nested = parent
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == OsStr::new(FORKS_DIR))
            && parent
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| super::ForkName::parse(name).is_ok());
        if nested {
            current = parent.to_path_buf();
            continue;
        }
        return Some((parent.to_path_buf(), fork));
    }
}

/// The nearest ancestor of `cwd` holding a `Forkfile` or `.slipfork` record.
///
/// A `<parent>/.forks/<fork>` checkout is never itself a root: resolution climbs to the project
/// that owns it.
#[must_use]
pub fn project_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        let project = dir.join("Forkfile").is_file() || dir.join(".slipfork").is_dir();
        if project && fork_checkout(dir).is_none() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// The deepest `<root>/.forks/<a>[/.forks/<b>…]` checkout containing `cwd`.
///
/// The walk extends the prefix only while a `.forks/<name>` pair exists as a directory and the
/// following component is another `.forks`, so both flat and legacy nested trees resolve to the
/// innermost checkout that actually exists. A root project named like a fork never matches,
/// because its parent is not `.forks`.
#[must_use]
pub fn fork_under(root: &Path, cwd: &Path) -> Option<(PathBuf, super::ForkName)> {
    let relative = cwd.strip_prefix(root).ok()?;
    let components: Vec<OsString> = relative
        .components()
        .map(|component| component.as_os_str().to_owned())
        .collect();
    let mut prefix = root.to_path_buf();
    let mut deepest = None;
    let mut index = 0;
    while index + 1 < components.len() {
        if components[index] != OsStr::new(FORKS_DIR) {
            break;
        }
        let fork = super::ForkName::parse(components[index + 1].to_str()?).ok()?;
        prefix = prefix.join(FORKS_DIR).join(fork.as_str());
        if !prefix.is_dir() {
            break;
        }
        deepest = Some((prefix.clone(), fork));
        index += 2;
    }
    deepest
}

/// One fork family: the outermost project and the world ids a fork is keyed against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkFamily {
    /// The outermost project directory owning `<root>/.forks/...`.
    pub root: PathBuf,
    /// `<root>`'s own world name; every fork world id is `<fork>--<root_world>`.
    pub root_world: WorldName,
    /// The immediate parent world id: `<fork>--<root_world>` when forking a fork, else the root.
    pub parent: WorldName,
}

/// Resolves the flat fork family for `project` and the world being forked.
///
/// `project` is either a project root or one of its `.forks/<name>` checkouts; the family root is
/// found structurally, so no `Forkfile`/`.slipfork` marker is required. A checkout's `world` must
/// be that checkout's own `<fork>--<root>` id: a canonical `<checkout>--<basename>` or, when the
/// root world is named differently, `<checkout>--<root>` with no further `--` in the root.
///
/// # Errors
///
/// [`Error::ForeignFamily`] when a checkout's `world` does not belong to that checkout's family,
/// so a legacy nested `<fork>--<parent>--<root>` id can never be re-compounded.
pub fn fork_family(project: &Path, world: &WorldName) -> Result<ForkFamily> {
    let Some((root, checkout)) = fork_checkout(project) else {
        return Ok(ForkFamily {
            root: project.to_path_buf(),
            root_world: world.clone(),
            parent: world.clone(),
        });
    };
    let basename = root.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let canonical = format!("{checkout}--{basename}");
    let foreign = || Error::ForeignFamily {
        checkout: project.to_path_buf(),
        world: world.to_string(),
        expected: canonical.clone(),
    };
    let root_world = if world.as_str() == canonical {
        WorldName::parse(basename).map_err(|_| foreign())?
    } else if let Some(rest) = world.as_str().strip_prefix(&format!("{checkout}--")) {
        if rest.contains("--") {
            return Err(foreign());
        }
        WorldName::parse(rest).map_err(|_| foreign())?
    } else {
        return Err(foreign());
    };
    Ok(ForkFamily {
        root,
        root_world,
        parent: world.clone(),
    })
}

/// Every `<project>/.forks/<name>` checkout present on disk.
///
/// A fork checkout is either a Btrfs subvolume (a fork of a subvolume checkout) or a plain copied
/// directory (a fork of a plain checkout), so any existing directory named like a fork is reported
/// instead of posing as a phantom stopped fork. A missing `.forks` directory is a normal project
/// with no forks; any other `read_dir` failure is surfaced to the caller.
///
/// # Errors
///
/// Returns the I/O failure from listing `<project>/.forks`, except a missing directory, which is
/// `Ok(Vec::new())`.
pub fn fork_checkouts(project: &Path) -> Result<Vec<ForkDir>> {
    let forks_dir = project_forks_dir(project);
    let entries = match std::fs::read_dir(&forks_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(Error::io("list", &forks_dir, source)),
    };
    let mut forks = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| Error::io("list", &forks_dir, source))?;
        let Some(name) = entry
            .file_name()
            .to_str()
            .and_then(|name| super::ForkName::parse(name).ok())
        else {
            continue;
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        forks.push(ForkDir {
            name: name.as_str().to_owned(),
            path,
        });
    }
    forks.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(forks)
}

/// `<fork>--<parent>`: the canonical world id for a fork of `parent`, validating both halves.
#[must_use]
pub fn fork_world_id(parent: &str, fork: &str) -> Option<WorldName> {
    let parent = WorldName::parse(parent).ok()?;
    let fork = super::ForkName::parse(fork).ok()?;
    Some(StoreLayout::fork_world_name(&parent, &fork))
}

/// Which project world a `ps` invocation is scoped to, resolved once from the caller's cwd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectScope {
    /// No project root above the cwd.
    None,
    /// The cwd sits inside one `<root>/.forks/<fork>` checkout.
    Fork {
        /// Absolute checkout path.
        path: PathBuf,
        /// Fork name.
        name: String,
        /// Canonical `<fork>--<parent>` world id.
        world: String,
    },
    /// The cwd sits in a project root: its own row plus every `.forks` checkout on disk.
    Root {
        /// Absolute project root.
        root: PathBuf,
        /// The root project's world id.
        world: String,
        /// Every fork checkout the host proves is a subvolume.
        forks: Vec<ForkDir>,
        /// Why `.forks` could not be listed, when it could not.
        problem: Option<String>,
    },
}

impl ProjectScope {
    /// Resolves `cwd` against the on-disk `.forks` layout exactly once.
    ///
    /// The `world` fields are display ids derived from the layout; a registered world's real
    /// identity always comes from its ledger row, never from this path derivation.
    #[must_use]
    pub fn resolve(cwd: &Path) -> Self {
        let Some(root) = project_root(cwd) else {
            return Self::None;
        };
        if let Some((path, fork)) = fork_under(&root, cwd) {
            let derived = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .and_then(|parent| fork_world_id(parent, fork.as_str()))
                .map_or_else(
                    || {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("world")
                            .to_owned()
                    },
                    |world| world.as_str().to_owned(),
                );
            return Self::Fork {
                world: derived,
                path,
                name: fork.as_str().to_owned(),
            };
        }
        let (forks, problem) = match fork_checkouts(&root) {
            Ok(forks) => (forks, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        Self::Root {
            world: root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("world")
                .to_owned(),
            root,
            forks,
            problem,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::{Builder, TempDir};

    use crate::error::Error;
    use crate::layout::{ForkName, StoreLayout, WorldName};

    use super::{fork_checkout, fork_family, fork_under, project_root};

    /// Real disk scratch space; `/tmp` is RAM-backed and must never hold a family tree.
    fn scratch(prefix: &str) -> TempDir {
        Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    fn root_project(base: &Path, name: &str) -> PathBuf {
        let root = base.join(name);
        fs::create_dir_all(&root).expect("create the project root");
        fs::write(root.join("Forkfile"), "# forkfile: 0.1\n").expect("seed the Forkfile");
        root
    }

    fn name(raw: &str) -> ForkName {
        ForkName::parse(raw).expect("valid fork name")
    }

    #[test]
    fn flat_checkout_resolves_the_family_root() {
        let tmp = scratch("slipfork-layout-flat");
        let root = root_project(tmp.path(), "a");
        let checkout = root.join(".forks/f1");
        fs::create_dir_all(&checkout).expect("create the flat checkout");

        assert_eq!(fork_checkout(&checkout), Some((root.clone(), name("f1"))));
        assert_eq!(
            fork_checkout(&root),
            None,
            "a plain root is never a checkout"
        );
        assert_eq!(project_root(&checkout), Some(root.clone()));
        assert_eq!(project_root(&root), Some(root.clone()));
        assert_eq!(
            StoreLayout::project_fork(&checkout, &name("f2")),
            root.join(".forks/f2"),
            "a fork of a fork lands flat beside its parent"
        );

        let world = WorldName::parse("f1--a").expect("valid world id");
        let family = fork_family(&checkout, &world).expect("resolve the flat family");
        assert_eq!(family.root, root);
        assert_eq!(
            family.root_world,
            WorldName::parse("a").expect("root world")
        );
        assert_eq!(family.parent, world);

        let root_world = WorldName::parse("a").expect("root world");
        let family = fork_family(&root, &root_world).expect("resolve at the root");
        assert_eq!(family.root, root);
        assert_eq!(family.parent, root_world);
        assert_eq!(family.root_world, root_world);
    }

    #[test]
    fn legacy_nested_tree_climbs_to_the_outermost_root() {
        let tmp = scratch("slipfork-layout-nested");
        let root = root_project(tmp.path(), "a");
        let first = root.join(".forks/f1");
        let second = first.join(".forks/f2");
        fs::create_dir_all(second.join("src")).expect("create the legacy nested tree");

        assert_eq!(fork_checkout(&second), Some((root.clone(), name("f2"))));
        assert_eq!(fork_checkout(&first), Some((root.clone(), name("f1"))));
        assert_eq!(project_root(&second), Some(root.clone()));

        assert_eq!(
            fork_under(&root, &second.join("src")),
            Some((second.clone(), name("f2"))),
            "fork_under returns the deepest existing checkout"
        );
        assert_eq!(
            fork_under(&root, &first.join("src")),
            Some((first.clone(), name("f1")))
        );
        assert_eq!(fork_under(&root, &root), None);

        let world = WorldName::parse("f2--a").expect("valid world id");
        let family = fork_family(&second, &world).expect("resolve the nested family");
        assert_eq!(family.root, root);
        assert_eq!(
            family.root_world,
            WorldName::parse("a").expect("root world")
        );

        let legacy = WorldName::parse("f2--f1--a").expect("a valid world id string");
        assert!(
            matches!(
                fork_family(&second, &legacy),
                Err(Error::ForeignFamily { .. })
            ),
            "a legacy compounded id never resolves to a new nested family"
        );
        let mismatched = WorldName::parse("other--a").expect("valid world id");
        assert!(
            matches!(
                fork_family(&second, &mismatched),
                Err(Error::ForeignFamily { .. })
            ),
            "a foreign world id fails closed with the dedicated error"
        );
    }

    #[test]
    fn fork_names_reserve_the_world_separator() {
        assert!(ForkName::parse("x--y").is_err());
        assert!(ForkName::parse("x-y").is_ok());
        assert!(WorldName::parse("x--y").is_ok(), "world ids keep `--`");
    }

    #[test]
    fn a_markerless_root_is_its_own_family() {
        let tmp = scratch("slipfork-layout-bare");
        let bare = tmp.path().join("bare");
        fs::create_dir_all(&bare).expect("create a markerless project");
        let world = WorldName::parse("bare").expect("valid world");
        let family = fork_family(&bare, &world).expect("resolve a markerless root");
        assert_eq!(family.root, bare);
        assert_eq!(family.root_world, world);
        assert_eq!(family.parent, world);
    }
}
