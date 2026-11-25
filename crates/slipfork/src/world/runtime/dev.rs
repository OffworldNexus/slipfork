//! Dev-only live mounts: each Dockerfile's target stage decides where its checkout is bound.
//!
//! The derivation itself lives in `slipfork-image` (the Dockerfile owner); this module only reads
//! those plain answers once per boot and exposes the refusal a dev world must fail closed with.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use slipfork_forkfile::FeatureSet;
use slipfork_forkfile::ast::Forkfile;
use slipfork_image::{BuildArgs, DockerfileParser, Stage, stage_walk};
use slipfork_world::ImageNode;

use crate::world::ImageHead;

/// The live checkout a dev pod mounts over its image working directory.
#[derive(Debug, Clone)]
pub(super) struct DevMount {
    /// The image's own `WORKDIR`, the single owner of the pod cwd.
    pub(super) workdir: String,
    /// Host build context bound at every [`Self::overlays`] entry.
    pub(super) context: PathBuf,
    /// Container directories the whole build context must overlay: the image `WORKDIR`, plus the
    /// destination of its whole-context `COPY`/`ADD` when that differs. Never `/`.
    pub(super) overlays: Vec<String>,
}

/// One image's dev verdict.
#[derive(Debug)]
enum Plan {
    /// Mount the live build context at the image's own working directory.
    Mount {
        /// Host build context (`<project>/<image build context>`).
        context: PathBuf,
        /// Destination of the stage's whole-context `COPY`/`ADD`, when it copied the context.
        context_dest: Option<String>,
        /// The image's fail-closed verdict, reused if its built workdir is unusable.
        refusal: String,
    },
    /// The stage's effective `WORKDIR` is `/`, which can never be overlaid.
    Refuse(String),
}

/// Per-image dev verdicts, derived once before the world boots.
#[derive(Debug, Default)]
pub(super) struct DevMounts {
    plans: BTreeMap<String, Plan>,
}

impl DevMounts {
    /// Derives one verdict per Dockerfile-built image under the active feature set.
    ///
    /// A Dockerfile that does not parse, or a `stage` target that names no stage, gets no entry:
    /// the builder reports that error itself, and an absent key means image-copy semantics.
    pub(super) fn load(
        project: &Path,
        world: &str,
        scheme: &str,
        images: &[ImageNode],
        doc: &Forkfile,
        features: &FeatureSet,
    ) -> Self {
        let mut plans = BTreeMap::new();
        if !features.contains("dev") {
            return Self { plans };
        }
        for node in images {
            if node.from.is_some() {
                continue;
            }
            let paths = node.paths(project);
            let Ok(source) = fs::read_to_string(&paths.dockerfile) else {
                continue;
            };
            let Ok(stages) = DockerfileParser::parse(&source) else {
                continue;
            };
            let target = doc.eval_image_stage_with(&node.id, features);
            let Ok(walk) = stage_walk(&stages, target.as_deref()) else {
                continue;
            };
            let mut args = BuildArgs::new();
            for (key, value) in doc.eval_image_args_with(world, scheme, &node.id, features) {
                args.declare(key, value);
            }
            let plan = match DockerfileParser::dev_stage(&stages, target.as_deref(), &args) {
                Ok(Some((_, context_dest))) => Plan::Mount {
                    context: paths.context,
                    context_dest,
                    refusal: refusal(
                        &node.id,
                        project,
                        &paths.dockerfile,
                        &stages[walk.end],
                        walk.end,
                    ),
                },
                Ok(None) => Plan::Refuse(refusal(
                    &node.id,
                    project,
                    &paths.dockerfile,
                    &stages[walk.end],
                    walk.end,
                )),
                Err(_) => continue,
            };
            plans.insert(node.id.clone(), plan);
        }
        Self { plans }
    }

    /// Refuses a dev boot before any pod launches when any active Dockerfile-built image runs
    /// from `/`, whether or not one of its services is declared.
    pub(super) fn preflight(&self) -> Result<(), String> {
        for plan in self.plans.values() {
            if let Plan::Refuse(message) = plan {
                return Err(message.clone());
            }
        }
        Ok(())
    }

    /// The live mount for one image, derived from the image's own built `WORKDIR`.
    ///
    /// An absent plan is image copy (pulled image, `-F -dev`, or a build error the builder owns).
    /// A built workdir that is not an absolute live directory, or a non-`/project` workdir with no
    /// overlay to carry the context, fails closed with the same one-line refusal.
    pub(super) fn dev_for(
        &self,
        image: &str,
        built: &ImageHead,
    ) -> Result<Option<DevMount>, String> {
        let (context, context_dest, refusal) = match self.plans.get(image) {
            None => return Ok(None),
            Some(Plan::Refuse(message)) => return Err(message.clone()),
            Some(Plan::Mount {
                context,
                context_dest,
                refusal,
            }) => (context, context_dest.as_deref(), refusal),
        };
        let workdir = normalize(&built.workdir);
        if !workdir.starts_with('/') || workdir == "/" {
            return Err(refusal.clone());
        }
        let overlays = overlay_destinations(&workdir, context_dest);
        if overlays.is_empty() && !under_project(&workdir) {
            return Err(refusal.clone());
        }
        Ok(Some(DevMount {
            workdir,
            context: context.clone(),
            overlays,
        }))
    }
}

/// Container directories the live context overlays: the stage `WORKDIR` unless it already sits
/// under the `/project` bind, then the whole-context `COPY` destination when it is a distinct,
/// absolute, non-root path outside that bind. Trailing slashes are normalised, order is stable,
/// duplicates are dropped, and relative/empty paths and `/` are never targets.
fn overlay_destinations(workdir: &str, context_dest: Option<&str>) -> Vec<String> {
    let workdir = normalize(workdir);
    let mut overlays = Vec::new();
    if live_destination(&workdir) && !under_project(&workdir) {
        overlays.push(workdir.clone());
    }
    if let Some(dest) = context_dest.map(normalize)
        && live_destination(&dest)
        && dest != workdir
        && !under_project(&dest)
        && !overlays.contains(&dest)
    {
        overlays.push(dest);
    }
    overlays
}

fn live_destination(path: &str) -> bool {
    path.starts_with('/') && path != "/"
}

fn under_project(path: &str) -> bool {
    path == "/project" || path.starts_with("/project/")
}

/// One spelling for comparisons and `/project` checks: absolute paths carry no trailing slash.
fn normalize(path: &str) -> String {
    if !path.starts_with('/') {
        return path.to_owned();
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// One line naming the image, project-relative Dockerfile, and stage, plus the only way forward.
fn refusal(image: &str, project: &Path, dockerfile: &Path, stage: &Stage, index: usize) -> String {
    let dockerfile = dockerfile.strip_prefix(project).unwrap_or(dockerfile);
    let stage = stage
        .alias
        .clone()
        .unwrap_or_else(|| format!("stage {}", index + 1));
    format!(
        "dev image `{image}` (`{}`, `{stage}`) has no WORKDIR (runs from `/`)\nnext: set WORKDIR to the directory that holds the copied context (e.g. WORKDIR /app) in the dev stage",
        dockerfile.display()
    )
}

#[cfg(test)]
mod tests {
    use super::overlay_destinations;

    #[test]
    fn the_workdir_and_the_context_copy_destination_are_both_overlaid() {
        assert_eq!(
            overlay_destinations("/app", Some("/srv")),
            ["/app".to_owned(), "/srv".to_owned()]
        );
        assert_eq!(
            overlay_destinations("/app", Some("/app")),
            ["/app".to_owned()]
        );
        assert_eq!(overlay_destinations("/app", Some("/")), ["/app".to_owned()]);
        assert_eq!(
            overlay_destinations("/project/ai", Some("/srv")),
            ["/srv".to_owned()]
        );
        assert!(overlay_destinations("/project", None).is_empty());
    }

    #[test]
    fn trailing_slashes_relative_and_root_destinations_never_duplicate_or_escape() {
        assert_eq!(
            overlay_destinations("/app/", Some("/app")),
            ["/app".to_owned()]
        );
        assert_eq!(
            overlay_destinations("/app", Some("/srv/")),
            ["/app".to_owned(), "/srv".to_owned()]
        );
        assert_eq!(
            overlay_destinations("app", Some("srv")),
            Vec::<String>::new()
        );
        assert_eq!(overlay_destinations("/app", Some("")), ["/app".to_owned()]);
    }
}
