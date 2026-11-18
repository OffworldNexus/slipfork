//! `docker buildx build` exporter with a local (`type=local`) output (`ADR-0009` §1.1).
//!
//! The builder hands a scratch `Dockerfile` and two named build contexts to [`DockerBuildx`]; the
//! concrete struct shells out to the real `docker buildx` binary and captures its stderr so a
//! failure is reported with the exact command context. Nothing else in the tree spawns
//! `docker buildx`.
//!
//! The `crun` + `slipfork-world-init` binaries reach the appended stage through the
//! `slipfork-runtime` **named build context** (`--build-context`), so Slipfork never writes into
//! the user's build context and never publishes (or pretends to publish) a runtime image.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use slipfork_world::rootfs::{BaseImageError, BuildEnvVar};

/// Name of the local build context carrying `/.slipfork/bin` payloads.
pub const SLIPFORK_RUNTIME_CONTEXT: &str = "slipfork-runtime";

/// One `docker buildx build` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildxRequest<'a> {
    /// Scratch `Dockerfile` outside the worktree (user bytes + appended `slipfork-world` stage).
    pub dockerfile: &'a Path,
    /// User build context (`snow.yml` `images[].context`).
    pub context_dir: &'a Path,
    /// Stage to export (`slipfork-world`).
    pub target: &'a str,
    /// Staging directory the local exporter writes to.
    pub dest_dir: &'a Path,
    /// `snow.yml` `images[].envs`, resolved by the Composition Root.
    pub build_envs: &'a [BuildEnvVar],
    /// Target platform (`linux/amd64`), derived from the requested architecture.
    pub platform: &'a str,
    /// Directory exposed as [`SLIPFORK_RUNTIME_CONTEXT`] (static `crun` + world init).
    pub runtime_context: &'a Path,
}

/// Concrete `docker buildx` exporter over the Docker CLI.
#[derive(Debug, Clone)]
pub struct DockerBuildx {
    program: PathBuf,
}

impl Default for DockerBuildx {
    fn default() -> Self {
        Self::new("docker")
    }
}

impl DockerBuildx {
    /// Use an explicit `docker` binary (live suites inject an explicit path).
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// The args this exporter passes to `docker buildx build`.
    pub fn args<'a>(&self, request: &BuildxRequest<'a>) -> Vec<String> {
        let mut args = vec![
            "buildx".to_owned(),
            "build".to_owned(),
            "--progress=plain".to_owned(),
            "--file".to_owned(),
            request.dockerfile.display().to_string(),
            "--target".to_owned(),
            request.target.to_owned(),
            "--build-context".to_owned(),
            format!(
                "{SLIPFORK_RUNTIME_CONTEXT}={}",
                request.runtime_context.display()
            ),
            "--output".to_owned(),
            format!("type=local,dest={}", request.dest_dir.display()),
        ];
        if !request.platform.is_empty() {
            args.push("--platform".to_owned());
            args.push(request.platform.to_owned());
        }
        for env in request.build_envs {
            // Pass the value through the child environment, never argv (`ps` would leak it).
            args.push("--build-arg".to_owned());
            args.push(env.name.clone());
        }
        args.push(request.context_dir.display().to_string());
        args
    }

    /// Build `request.target` from `request.dockerfile` into `request.dest_dir`.
    pub fn build_local(&self, request: &BuildxRequest<'_>) -> Result<(), BaseImageError> {
        let mut command = Command::new(&self.program);
        command.args(self.args(request));
        for env in request.build_envs {
            command.env(&env.name, &env.value);
        }
        let output = command.output().map_err(|error| BaseImageError::Buildx {
            reason: format!(
                "`{}` could not be started ({error}); is Docker installed with the buildx plugin?",
                self.program.display()
            ),
        })?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        Err(BaseImageError::Buildx {
            reason: format!(
                "`docker buildx build` exited with {}: {tail}",
                output.status
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{BuildxRequest, DockerBuildx, SLIPFORK_RUNTIME_CONTEXT};
    use slipfork_world::rootfs::BuildEnvVar;

    #[test]
    fn args_export_only_the_slipfork_world_target() {
        let args = DockerBuildx::default().args(&BuildxRequest {
            dockerfile: Path::new("/state/cache/tmp/Dockerfile.1"),
            context_dir: Path::new("/repo"),
            target: "slipfork-world",
            dest_dir: Path::new("/state/cache/rootfs/x"),
            build_envs: &[],
            platform: "linux/amd64",
            runtime_context: Path::new("/state/cache/tmp/runtime.1"),
        });
        assert!(args.windows(2).any(|w| w == ["--target", "slipfork-world"]));
        assert!(args.contains(&format!(
            "{SLIPFORK_RUNTIME_CONTEXT}=/state/cache/tmp/runtime.1"
        )));
        assert!(
            args.iter()
                .any(|arg| arg == "type=local,dest=/state/cache/rootfs/x")
        );
        assert_eq!(args.last().map(String::as_str), Some("/repo"));
        assert!(!args.iter().any(|arg| arg.contains("secret")));
    }

    #[test]
    fn build_envs_never_reach_argv() {
        let envs = [BuildEnvVar {
            name: "NPM_TOKEN".to_owned(),
            value: "super-secret".to_owned(),
        }];
        let args = DockerBuildx::default().args(&BuildxRequest {
            dockerfile: Path::new("/state/Dockerfile"),
            context_dir: Path::new("/repo"),
            target: "slipfork-world",
            dest_dir: Path::new("/state/out"),
            build_envs: &envs,
            platform: "",
            runtime_context: Path::new("/state/runtime"),
        });
        assert!(args.windows(2).any(|w| w == ["--build-arg", "NPM_TOKEN"]));
        assert!(!args.iter().any(|arg| arg.contains("super-secret")));
    }
}
