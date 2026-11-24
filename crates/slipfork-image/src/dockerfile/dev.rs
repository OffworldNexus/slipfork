//! `dev` live-mount derivation: the target stage's effective `WORKDIR` and where its whole build
//! context landed.
//!
//! This mirrors the builder's own `ARG`/`ENV` expansion instead of reading raw instruction text,
//! so `ARG APP=/app` + `WORKDIR $APP` resolves to `/app` exactly as the image runs it. Callers
//! never re-scan Dockerfile text, join `WORKDIR` values, or guess a mount from a `COPY`
//! destination.

use crate::builder::stage_walk;
use crate::error::{Error, Result};

use super::config::ImageConfig;
use super::instruction::{Instruction, Stage, StageBase};
use super::variables::{BuildArgs, Variables};

impl super::DockerfileParser {
    /// The live stage a `dev` pod runs: the target stage's effective `WORKDIR` plus the destination
    /// of its last whole-context `COPY`/`ADD`.
    ///
    /// Stages are evaluated up to `stage_walk`'s target exactly like the builder: a `FROM <image>`
    /// base starts from a default [`ImageConfig`], a `FROM <stage>` base clones that stage's final
    /// config, `ARG` declares the request value or its default, and `ENV`/`WORKDIR`/`COPY` values
    /// are expanded with [`Variables`] before they are applied.
    ///
    /// * `Err` — the `stage <name>` target names no stage; the builder reports it itself.
    /// * `Ok(None)` — the effective `WORKDIR` is still `/`, which can never be overlaid.
    /// * `Ok(Some((workdir, context_dest)))` — the live mount to derive.
    pub fn dev_stage(
        stages: &[Stage],
        target: Option<&str>,
        args: &BuildArgs,
    ) -> Result<Option<(String, Option<String>)>> {
        let walk = stage_walk(stages, target)?;
        let mut built: Vec<(Option<String>, ImageConfig, Option<String>)> = Vec::new();
        let mut local_args = args.clone();
        for stage in &stages[..=walk.end] {
            let (mut config, mut context_dest) = match &stage.base {
                StageBase::Image(_) => (ImageConfig::default(), None),
                StageBase::Stage(base) => {
                    let (_, config, dest) = built
                        .iter()
                        .find(|(alias, ..)| alias.as_deref() == Some(base.as_str()))
                        .ok_or_else(|| Error::UnknownStage { name: base.clone() })?;
                    (config.clone(), dest.clone())
                }
            };
            for parsed in &stage.instructions {
                match &parsed.instruction {
                    Instruction::Arg { name, default } => {
                        let value = args
                            .get(name)
                            .map(str::to_owned)
                            .or_else(|| default.clone())
                            .unwrap_or_default();
                        local_args.declare(name, value);
                    }
                    Instruction::Env(pairs) => {
                        let variables = Variables::new(&local_args, &config.env);
                        for (key, value) in pairs {
                            config.env.insert(key.clone(), variables.expand(value));
                        }
                    }
                    Instruction::Workdir(directory) => {
                        let variables = Variables::new(&local_args, &config.env);
                        let _ = config.set_workdir(&variables.expand(directory));
                    }
                    Instruction::Copy(spec) | Instruction::Add(spec) => {
                        let variables = Variables::new(&local_args, &config.env);
                        let sources: Vec<String> = spec
                            .sources
                            .iter()
                            .map(|source| variables.expand(source))
                            .collect();
                        if spec.from.is_none() && copies_context(&sources) {
                            context_dest =
                                Some(resolve_dest(&config.workdir, &variables.expand(&spec.dest)));
                        }
                    }
                    _ => {}
                }
            }
            built.push((stage.alias.clone(), config, context_dest));
        }
        // `stage_walk` proved `walk.end` is in range, so the target stage always exists.
        let (_, config, context_dest) = built.last().expect("stage_walk bounds the walk");
        let workdir = normalize(&config.workdir);
        if !workdir.starts_with('/') || workdir == "/" {
            return Ok(None);
        }
        Ok(Some((workdir, context_dest.as_deref().map(normalize))))
    }
}

/// `COPY .` / `COPY ./` is the whole build context; anything narrower is a partial copy.
fn copies_context(sources: &[String]) -> bool {
    sources.len() == 1 && matches!(sources[0].as_str(), "." | "./")
}

fn resolve_dest(workdir: &str, dest: &str) -> String {
    if dest == "." || dest == "./" {
        return normalize(workdir);
    }
    if dest.starts_with('/') {
        return normalize(dest);
    }
    normalize(&format!(
        "{}/{}",
        workdir.trim_end_matches('/'),
        dest.trim_end_matches('/')
    ))
}

/// One spelling for comparisons and pod `/project` checks: absolute paths carry no trailing slash.
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

#[cfg(test)]
mod tests {
    use super::super::DockerfileParser;
    use super::super::variables::BuildArgs;

    fn parse(source: &str) -> Vec<super::super::Stage> {
        DockerfileParser::parse(source).expect("parse the fixture Dockerfile")
    }

    fn resolved(
        stages: &[super::super::Stage],
        target: Option<&str>,
        args: &BuildArgs,
    ) -> (String, Option<String>) {
        DockerfileParser::dev_stage(stages, target, args)
            .expect("resolve the dev stage")
            .expect("the stage has a live workdir")
    }

    #[test]
    fn workdir_and_copy_dest_expand_declared_args_like_the_builder() {
        let stages = parse("FROM python:3.13-slim\nARG APP=/app\nWORKDIR $APP\nCOPY . .\n");
        let (workdir, dest) = resolved(&stages, None, &BuildArgs::new());
        assert_eq!(workdir, "/app");
        assert_eq!(dest.as_deref(), Some("/app"));

        let mut args = BuildArgs::new();
        args.declare("APP", "/opt/app");
        let (workdir, _) = resolved(&stages, None, &args);
        assert_eq!(workdir, "/opt/app", "a declared request value wins");
    }

    #[test]
    fn an_unset_workdir_variable_is_not_absolute_and_an_explicit_copy_dest_is_kept() {
        let stages = parse("FROM python:3.13-slim\nARG DIR\nWORKDIR $DIR\nCOPY . .\n");
        assert_eq!(
            DockerfileParser::dev_stage(&stages, None, &BuildArgs::new())
                .expect("an unset workdir still resolves"),
            None,
            "a relative/unexpanded workdir is refused, never mounted"
        );

        let stages = parse("FROM python:3.13-slim\nWORKDIR /app\nCOPY . /srv\n");
        let (workdir, dest) = resolved(&stages, None, &BuildArgs::new());
        assert_eq!(workdir, "/app");
        assert_eq!(dest.as_deref(), Some("/srv"));
    }

    #[test]
    fn env_values_expand_the_copy_destination_and_the_workdir() {
        let stages = parse("FROM python:3.13-slim\nENV ROOT=/srv\nWORKDIR /app\nCOPY . $ROOT\n");
        let (workdir, dest) = resolved(&stages, None, &BuildArgs::new());
        assert_eq!(workdir, "/app");
        assert_eq!(dest.as_deref(), Some("/srv"));
    }

    #[test]
    fn an_explicit_root_workdir_is_not_a_live_stage_and_unknown_targets_error() {
        let stages = parse("FROM python:3.13-slim\nWORKDIR /\nCOPY . .\n");
        assert_eq!(
            DockerfileParser::dev_stage(&stages, None, &BuildArgs::new()).expect("root resolves"),
            None
        );

        let stages = parse("FROM python:3.13-slim\nWORKDIR /app\nCOPY . .\n");
        assert!(
            DockerfileParser::dev_stage(&stages, Some("missing"), &BuildArgs::new()).is_err(),
            "an unknown target belongs to the builder's error path"
        );
    }

    #[test]
    fn the_selected_target_stage_inherits_workdir_and_context_from_an_earlier_stage() {
        let stages = parse(
            "FROM python:3.13-slim AS build\nWORKDIR /app\nCOPY . .\n\
             FROM build AS dev\nWORKDIR src\n",
        );
        let (workdir, dest) = resolved(&stages, Some("dev"), &BuildArgs::new());
        assert_eq!(workdir, "/app/src");
        assert_eq!(dest.as_deref(), Some("/app"));
    }
}
