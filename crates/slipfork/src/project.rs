//! The project identity adapter and the crate's single `Forkfile` loader.
//!
//! `slipfork` and `slipforkd` both resolve project identity here from the `Forkfile`'s
//! `project <slug>` statement (falling back to the Git origin inside `slipfork-vcs`); there is no
//! second manifest and no TOML parser in this crate.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipfork_edge::EdgeError;
use slipfork_forkfile::{
    Diagnostic, EvaluatedFork, EvaluatedImage, EvaluatedService, FeatureSet, ForkFile, Stmt,
    WORLD_FEATURE, parse_forkfile,
};
use slipfork_vcs::{
    GitRepoRootResolver, ProjectSlugOverride, RepoRoot, RepoRootResolver, VcsError,
    WORKTREES_DIRECTORY,
};
use slipfork_wire::control::{
    DaemonError, DaemonErrorKind, ExecService, FeatureSelection, WorldSelector, WorldSelectorRepo,
};
use slipfork_world::{ProjectSlug, WorldTarget};

/// The one repository-resolver constructor: `ProjectSlugOverride` + `GitRepoRootResolver`.
pub fn repository_resolver() -> Arc<dyn RepoRootResolver> {
    Arc::new(GitRepoRootResolver::new(Arc::new(ForkfileProjectOverride)))
}

/// Load, statically check, and feature-evaluate `<worktree>/Forkfile` for one invocation.
///
/// `local` is the manifest's default world feature; `selection.no_default_features` drops it and
/// the requested `-F` features are evaluated on top, so an unknown feature fails closed inside
/// [`ForkFile::evaluate`] with the diagnostic's own one-line `next:` remedy.
pub fn load_project_fork(
    worktree: &Path,
    selection: &FeatureSelection,
) -> Result<EvaluatedFork, String> {
    let path = worktree.join("Forkfile");
    let source = fs::read_to_string(&path).map_err(|_| missing_forkfile(&path))?;
    let file = parse_forkfile(&source).map_err(|diagnostics| refusal(&path, &diagnostics))?;
    file.evaluate(&active_features(&file, selection)?)
        .map_err(|diagnostics| refusal(&path, &diagnostics))
}

/// Map one manifest refusal onto the daemon's control-error surface.
pub fn forkfile_error(diagnostics: Vec<Diagnostic>) -> EdgeError {
    let (kind, line) = match diagnostics.into_iter().next() {
        Some(diagnostic) => (
            DaemonErrorKind::Rejected,
            format!(
                "{}{}",
                diagnostic.message,
                remedy(diagnostic.hint.as_deref())
            ),
        ),
        None => (
            DaemonErrorKind::Rejected,
            format!(
                "the Forkfile could not be resolved{}",
                remedy(Some("check the `Forkfile` at the repository root"))
            ),
        ),
    };
    EdgeError::Control(DaemonError::new(kind, line))
}

/// The one `next:` remedy: the diagnostic's own hint, or the generic syntax fix.
fn remedy(hint: Option<&str>) -> String {
    match hint {
        Some(hint) => format!("\n  next: {hint}"),
        None => {
            "\n  next: fix the `Forkfile` syntax at that span, then re-run the command".to_owned()
        }
    }
}

/// The `project <slug>` a `Forkfile` declares, if any.
pub fn declared_project(file: &ForkFile) -> Option<&str> {
    file.statements
        .iter()
        .find_map(|statement| match statement {
            Stmt::Project(project) => Some(project.value.as_str()),
            _ => None,
        })
}

/// The checkout `--world` selects, using the same `<project>` / `<project>:<fork>` / `<fork>`
/// grammar the daemon resolves.
///
/// `world = None` means the checkout containing the caller (`worktree_root`), so an `env` inside
/// `.worktrees/<fork>` reads that fork's own manifest.
pub fn world_worktree(repo: &RepoRoot, world: Option<&str>) -> Result<PathBuf, String> {
    let Some(raw) = world else {
        return Ok(repo.worktree_root.clone());
    };
    let target = WorldSelector::parse(
        raw,
        WorldSelectorRepo {
            project: &repo.project,
            vcs_root: &repo.vcs_root,
            worktree_root: &repo.worktree_root,
        },
    )?;
    match target {
        WorldTarget::Canonical => Ok(repo.vcs_root.clone()),
        WorldTarget::UserFork(fork) => Ok(repo.vcs_root.join(WORKTREES_DIRECTORY).join(fork)),
        WorldTarget::TestSlot(_) => Err(format!(
            "world selector '{raw}' names a test slot; `--world` accepts only `<project>`, `<project>:<fork>`, or `<fork>`"
        )),
    }
}

/// One service's directory relative to the worktree root (`None` for a pulled OCI image).
pub fn service_directory(service: &EvaluatedService) -> PathBuf {
    match &service.image {
        EvaluatedImage::Dockerfile { context_dir, .. } => PathBuf::from(context_dir),
        EvaluatedImage::OciImage { .. } => PathBuf::new(),
    }
}

/// The app service `cwd` selects, failing closed at an ambiguous repo root.
///
/// An explicit `requested` id wins; otherwise a single declared service is the answer, a `cwd`
/// inside `<worktree>/<service_dir>` selects that service, and a multi-service checkout otherwise
/// returns the candidate ids with the caller's own `remedy` (the verb that knows its own flag:
/// `exec -s`, `shell <id>`, `env -s`).
pub fn select_service(
    fork: &EvaluatedFork,
    worktree: &Path,
    cwd: &Path,
    requested: Option<&str>,
    remedy: &dyn Fn(&str) -> String,
) -> Result<Option<String>, String> {
    if let Some(id) = requested {
        if fork.services.iter().any(|service| service.id == id) {
            return Ok(Some(id.to_owned()));
        }
        return Err(format!(
            "the Forkfile declares no service '{id}'\n  next: use one of the declared services ({})",
            service_ids(fork)
        ));
    }
    let Some(first) = fork.services.first() else {
        return Ok(None);
    };
    if fork.services.len() == 1 {
        return Ok(Some(first.id.clone()));
    }
    let mut best: Option<(usize, &str)> = None;
    for service in &fork.services {
        let directory = service_directory(service);
        if directory.as_os_str().is_empty() {
            continue;
        }
        let root = worktree.join(&directory);
        if cwd.starts_with(&root) {
            let depth = root.as_os_str().len();
            if best.is_none_or(|(current, _)| depth > current) {
                best = Some((depth, service.id.as_str()));
            }
        }
    }
    if let Some((_, id)) = best {
        return Ok(Some(id.to_owned()));
    }
    Err(format!(
        "multiple services ({})\n  next: {}",
        service_ids(fork),
        remedy(first.id.as_str())
    ))
}

fn service_ids(fork: &EvaluatedFork) -> String {
    let ids: Vec<&str> = fork
        .services
        .iter()
        .map(|service| service.id.as_str())
        .collect();
    if ids.is_empty() {
        "none".to_owned()
    } else {
        ids.join(", ")
    }
}

/// The canonical entry one `exec`/`shell`/`run` call selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecEntry {
    /// The world's app stage with no declared service for this `cwd` (the plain world rootfs).
    World,
    /// One declared app service, entered at its own directory.
    App(String),
    /// One declared backing resource, entered in its running container.
    Backing(String),
}

/// Resolve one typed [`ExecService`] against the world's own `Forkfile`.
///
/// This is the forkfile-owned lookup: an app service goes through [`select_service`] (one
/// ambiguity/refusal owner), a backing id through [`resolve_backing_service`], and
/// [`ExecService::World`] auto-selects the service `cwd` belongs to. Nothing classifies an id by
/// its spelling.
pub fn select_exec_entry(
    fork: &EvaluatedFork,
    worktree: &Path,
    cwd: &Path,
    service: &ExecService,
) -> Result<ExecEntry, String> {
    match service {
        ExecService::World => Ok(select_service(fork, worktree, cwd, None, &|id| {
            format!("pass -s/--service {id}, or run from that service's directory")
        })?
        .map_or(ExecEntry::World, ExecEntry::App)),
        ExecService::App(id) => {
            select_service(fork, worktree, cwd, Some(id), &|candidate| {
                format!("pass -s/--service {candidate}")
            })?;
            Ok(ExecEntry::App(id.clone()))
        }
        ExecService::Backing(id) => Ok(ExecEntry::Backing(id.clone())),
    }
}

/// Resolve one declared backing-resource id to the running container id and the argv to run.
///
/// `pg_databases.main` selects the `pg_clusters.<cluster>` container and appends `-d main` to a
/// bare `psql`; `redis_clusters.cache` and `pg_clusters.<id>` are their own service. Nothing ever
/// falls back to the app rootfs.
pub fn resolve_backing_service(
    fork: &EvaluatedFork,
    requested: &str,
    argv: &[String],
) -> Result<(String, Vec<String>), String> {
    if requested.trim().is_empty() {
        return Err("a backing-service id must not be empty".to_owned());
    }
    let (kind, id) = requested.split_once('.').ok_or_else(|| {
        format!(
            "invalid backing-service id '{requested}' (expected <kind>.<id>, for example pg_databases.main)"
        )
    })?;
    match kind {
        "pg_databases" => {
            let database = fork
                .pg_databases()
                .into_iter()
                .find(|database| database.database == id)
                .ok_or_else(|| unknown_backing_service(fork, requested))?;
            let mut argv = argv.to_vec();
            if argv.first().map(String::as_str) == Some("psql")
                && !argv.iter().any(|arg| arg == "-d" || arg == "--dbname")
            {
                argv.push("-d".to_owned());
                argv.push(database.database.clone());
            }
            Ok((format!("pg_clusters.{}", database.cluster), argv))
        }
        "redis_clusters" => {
            if !fork.redis.iter().any(|cluster| cluster.id == id) {
                return Err(unknown_backing_service(fork, requested));
            }
            Ok((requested.to_owned(), argv.to_vec()))
        }
        "pg_clusters" => {
            if !fork.postgres.iter().any(|cluster| cluster.id == id) {
                return Err(unknown_backing_service(fork, requested));
            }
            Ok((requested.to_owned(), argv.to_vec()))
        }
        _ => Err(unknown_backing_service(fork, requested)),
    }
}

/// One actionable refusal naming every backing resource the manifest declares.
fn unknown_backing_service(fork: &EvaluatedFork, requested: &str) -> String {
    let mut known: Vec<String> = fork
        .pg_databases()
        .into_iter()
        .map(|database| format!("pg_databases.{}", database.database))
        .chain(
            fork.redis
                .iter()
                .map(|cluster| format!("redis_clusters.{}", cluster.id)),
        )
        .collect();
    known.sort();
    format!(
        "world declares no backing service '{requested}' (known services: {})",
        if known.is_empty() {
            "none".to_owned()
        } else {
            known.join(", ")
        }
    )
}

/// The feature selection for one world: `local` unless `--no-default-features`, plus every `-F`.
fn active_features(file: &ForkFile, selection: &FeatureSelection) -> Result<FeatureSet, String> {
    if !selection.no_default_features && !file.declares_feature(WORLD_FEATURE) {
        return Err(format!(
            "this `Forkfile` declares no `{WORLD_FEATURE}` feature\n  next: wrap world statements in a `features local {{ ... }}` block, or pass `--no-default-features -F <feature>`"
        ));
    }
    let mut set = FeatureSet::from_cli(&selection.features, selection.no_default_features);
    if !selection.no_default_features {
        set.features.insert(WORLD_FEATURE.to_owned());
    }
    Ok(set)
}

/// The one missing-manifest remedy the loader and the project override both print.
fn missing_forkfile(path: &Path) -> String {
    format!(
        "cannot read {}; next: add the project `Forkfile` at the repository root",
        path.display()
    )
}

/// One actionable line plus the single remedy, with no synthesized-source rustc block.
fn refusal(path: &Path, diagnostics: &[Diagnostic]) -> String {
    let first = &diagnostics[0];
    format!(
        "{}:{}:{}: {}{}",
        path.display(),
        first.span.line,
        first.span.col,
        first.message,
        remedy(first.hint.as_deref())
    )
}

/// Reads `<vcs_root>/Forkfile`'s `project <slug>` through the one canonical parser.
#[derive(Debug, Default, Clone, Copy)]
pub struct ForkfileProjectOverride;

impl ProjectSlugOverride for ForkfileProjectOverride {
    fn project_override(&self, vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError> {
        let path = vcs_root.join("Forkfile");
        let source = match fs::read_to_string(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(VcsError::Io {
                    message: missing_forkfile(&path),
                    path,
                });
            }
            Ok(source) => source,
        };
        // A refusal carries its `path:line:col` and `next:` remedy: `slipfork ps` and boot print it.
        let file = parse_forkfile(&source).map_err(|diagnostics| VcsError::Io {
            message: refusal(&path, &diagnostics),
            path: path.clone(),
        })?;
        let Some(slug) = declared_project(&file) else {
            return Ok(None);
        };
        ProjectSlug::parse(slug)
            .map(Some)
            .map_err(|err| VcsError::Io {
                path,
                message: err.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_forkfile_means_no_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            ForkfileProjectOverride
                .project_override(dir.path())
                .expect("a missing Forkfile is not an error"),
            None
        );
    }

    #[test]
    fn project_statement_is_read_through_the_one_parser() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("Forkfile"), "project acme\n").expect("write Forkfile");
        assert_eq!(
            ForkfileProjectOverride
                .project_override(dir.path())
                .expect("valid Forkfile"),
            Some(ProjectSlug::parse("acme").expect("valid slug"))
        );
    }

    #[test]
    fn invalid_project_slug_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("Forkfile"), "project Bad_Slug\n").expect("write Forkfile");
        let err = ForkfileProjectOverride
            .project_override(dir.path())
            .expect_err("invalid slug must fail closed");
        assert!(err.to_string().contains("Forkfile"), "got: {err}");
    }

    #[test]
    fn a_forkfile_without_the_local_feature_is_refused() {
        let dir = tempfile::Builder::new()
            .prefix("slipfork-project")
            .tempdir_in("/var/tmp")
            .expect("disk scratch");
        fs::write(dir.path().join("Forkfile"), "project acme\n").expect("write Forkfile");
        let message = load_project_fork(dir.path(), &FeatureSelection::default())
            .expect_err("a world-less Forkfile refuses");
        let remedy = "next: wrap world statements in a `features local { ... }` block";
        assert!(
            message.contains("declares no `local` feature"),
            "got: {message}"
        );
        assert!(message.contains(remedy), "got: {message}");
    }
}

#[cfg(test)]
mod feature_selection_tests {
    use slipfork_wire::control::FeatureSelection;

    use super::*;

    /// A manifest that declares two features and one serviced world.
    const TWO_FEATURES: &str = "\
project demo

features {
    local
    extra
}

service api ./api {
    port 3000
    run /bin/true
}
";

    fn manifest() -> tempfile::TempDir {
        let dir = tempfile::tempdir_in("/var/tmp").expect("a /var/tmp scratch directory");
        std::fs::write(dir.path().join("Forkfile"), TWO_FEATURES).expect("write Forkfile");
        dir
    }

    #[test]
    fn an_undeclared_f_feature_is_refused_in_one_line_with_a_next_remedy() {
        let dir = manifest();
        let error = load_project_fork(
            dir.path(),
            &FeatureSelection {
                features: vec!["nope".to_owned()],
                ..FeatureSelection::default()
            },
        )
        .expect_err("an undeclared -F feature must fail closed");

        assert_eq!(
            error.lines().filter(|line| !line.is_empty()).count(),
            2,
            "one actionable line plus one next: remedy: {error}"
        );
        assert!(
            error.contains("feature `nope` is not declared"),
            "the refusal names the feature: {error}"
        );
        assert!(
            error.contains("\n  next: "),
            "the refusal carries the remedy: {error}"
        );
    }

    #[test]
    fn no_default_features_drops_local_and_keeps_the_requested_feature() {
        let dir = manifest();
        let default = load_project_fork(dir.path(), &FeatureSelection::default())
            .expect("the manifest declares its local world feature");
        assert_eq!(default.features, vec!["local".to_owned()]);

        let dropped = load_project_fork(
            dir.path(),
            &FeatureSelection {
                no_default_features: true,
                ..FeatureSelection::default()
            },
        )
        .expect("the default feature set is optional");
        assert!(
            !dropped
                .features
                .iter()
                .any(|feature| feature == WORLD_FEATURE),
            "`--no-default-features` drops `local`: {:?}",
            dropped.features
        );

        let extra = load_project_fork(
            dir.path(),
            &FeatureSelection {
                features: vec!["extra".to_owned()],
                no_default_features: true,
                ..FeatureSelection::default()
            },
        )
        .expect("`extra` is declared");
        assert_eq!(extra.features, vec!["extra".to_owned()]);
    }
}

#[cfg(test)]
mod selector_tests {
    use super::*;

    const MULTI: &str = "\
project acme

features {
    local
}

service web ./web {
    port 3000
}

service api ./web/api {
    port 3001
}
";

    const OCI_MIXED: &str = "\
project acme

features {
    local
}

service cache from redis:8 {
    port 6379
}

service api ./api {
    port 3000
}
";

    const NO_SERVICE: &str = "\
project acme

features {
    local
}
";

    fn fork_of(body: &str) -> (tempfile::TempDir, EvaluatedFork) {
        let dir = tempfile::Builder::new()
            .prefix("slipfork-select")
            .tempdir_in("/var/tmp")
            .expect("disk scratch");
        fs::write(dir.path().join("Forkfile"), body).expect("write Forkfile");
        let fork = load_project_fork(dir.path(), &FeatureSelection::default())
            .expect("a valid evaluated manifest");
        (dir, fork)
    }

    fn repo(tmp: &Path) -> RepoRoot {
        RepoRoot {
            vcs_root: tmp.join("vcs"),
            worktree_root: tmp.join("vcs").join(WORKTREES_DIRECTORY).join("feat"),
            project: ProjectSlug::parse("acme").expect("valid project slug"),
        }
    }

    #[test]
    fn world_worktree_shares_the_one_selector_grammar() {
        let tmp = tempfile::Builder::new()
            .prefix("slipfork-world")
            .tempdir_in("/var/tmp")
            .expect("disk scratch");
        let repo = repo(tmp.path());
        let fork_checkout = repo.vcs_root.join(WORKTREES_DIRECTORY).join("feat");

        assert_eq!(
            world_worktree(&repo, None).expect("the containing checkout"),
            repo.worktree_root
        );
        assert_eq!(
            world_worktree(&repo, Some("acme")).expect("the canonical world"),
            repo.vcs_root
        );
        assert_eq!(
            world_worktree(&repo, Some("feat")).expect("a bare fork"),
            fork_checkout
        );
        assert_eq!(
            world_worktree(&repo, Some("acme:feat")).expect("a project-qualified fork"),
            fork_checkout
        );

        let empty = world_worktree(&repo, Some("")).expect_err("empty selector fails closed");
        assert!(empty.contains("must not be empty"), "got: {empty}");
        let extra = world_worktree(&repo, Some("acme:a:b")).expect_err("two ':' fail closed");
        assert!(extra.contains("at most one ':'"), "got: {extra}");
        let foreign = world_worktree(&repo, Some("other:feat")).expect_err("foreign project");
        assert!(
            foreign.contains("belongs to project 'other'"),
            "got: {foreign}"
        );
    }

    #[test]
    fn service_directory_is_the_build_context_or_empty_for_a_pulled_image() {
        let (_dir, fork) = fork_of(MULTI);
        assert_eq!(service_directory(&fork.services[0]), PathBuf::from("web"));
        assert_eq!(
            service_directory(&fork.services[1]),
            PathBuf::from("web/api")
        );

        let (_oci, fork) = fork_of(OCI_MIXED);
        let cache = fork
            .services
            .iter()
            .find(|service| service.id == "cache")
            .expect("the pulled service");
        assert_eq!(service_directory(cache), PathBuf::new());
    }

    fn remedy(id: &str) -> String {
        format!("slipfork shell {id}")
    }

    #[test]
    fn select_service_prefers_the_deepest_cwd_then_refuses_ambiguity() {
        let (dir, fork) = fork_of(MULTI);
        let root = dir.path();
        assert_eq!(
            select_service(&fork, root, &root.join("web"), None, &remedy).expect("web"),
            Some("web".to_owned())
        );
        assert_eq!(
            select_service(&fork, root, &root.join("web/api"), None, &remedy).expect("depth"),
            Some("api".to_owned())
        );
        let ambiguous =
            select_service(&fork, root, root, None, &remedy).expect_err("repo root is ambiguous");
        assert!(ambiguous.contains("multiple services"), "got: {ambiguous}");
        assert!(
            ambiguous.contains("\n  next: slipfork shell web"),
            "the caller's remedy must be the verb's own shape: {ambiguous}"
        );
        assert_eq!(
            select_service(&fork, root, root, Some("api"), &remedy).expect("explicit id wins"),
            Some("api".to_owned())
        );
        let unknown =
            select_service(&fork, root, root, Some("nope"), &remedy).expect_err("unknown id");
        assert!(
            unknown.contains("declares no service 'nope'"),
            "got: {unknown}"
        );
    }

    #[test]
    fn select_service_skips_an_oci_services_empty_directory_and_handles_no_services() {
        let (dir, fork) = fork_of(OCI_MIXED);
        let error = select_service(&fork, dir.path(), dir.path(), None, &remedy)
            .expect_err("an empty OCI directory must not match the repo root");
        assert!(error.contains("multiple services"), "got: {error}");

        let (empty_dir, fork) = fork_of(NO_SERVICE);
        assert_eq!(
            select_service(&fork, empty_dir.path(), empty_dir.path(), None, &remedy)
                .expect("a service-less manifest"),
            None
        );
    }

    #[test]
    fn select_exec_entry_resolves_both_surfaces_without_classifying_ids() {
        let (dir, fork) = fork_of(OCI_MIXED);
        let root = dir.path();
        assert_eq!(
            select_exec_entry(&fork, root, &root.join("api"), &ExecService::World)
                .expect("auto-selected app service"),
            ExecEntry::App("api".to_owned())
        );
        assert_eq!(
            select_exec_entry(&fork, root, root, &ExecService::App("cache".to_owned()))
                .expect("a declared OCI app service"),
            ExecEntry::App("cache".to_owned())
        );
        assert_eq!(
            select_exec_entry(
                &fork,
                root,
                root,
                &ExecService::Backing("redis_clusters.cache".to_owned())
            )
            .expect("a declared backing id"),
            ExecEntry::Backing("redis_clusters.cache".to_owned())
        );
        let unknown = select_exec_entry(&fork, root, root, &ExecService::App("nope".to_owned()))
            .expect_err("an unknown app service fails closed");
        assert!(
            unknown.contains("declares no service 'nope'"),
            "got: {unknown}"
        );
    }
}
