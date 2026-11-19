//! The project identity adapter and the crate's single `Forkfile` loader.
//!
//! `slipfork` and `slipforkd` both resolve project identity here from the `Forkfile`'s
//! `project <slug>` statement (falling back to the Git origin inside `slipfork-vcs`); there is no
//! second manifest and no TOML parser in this crate.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use slipfork_edge::EdgeError;
use slipfork_forkfile::{
    Diagnostic, EvaluatedFork, FeatureSet, ForkFile, Stmt, WORLD_FEATURE, parse_forkfile,
};
use slipfork_vcs::{GitRepoRootResolver, ProjectSlugOverride, RepoRootResolver, VcsError};
use slipfork_wire::control::{DaemonError, DaemonErrorKind, FeatureSelection};
use slipfork_world::ProjectSlug;

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
