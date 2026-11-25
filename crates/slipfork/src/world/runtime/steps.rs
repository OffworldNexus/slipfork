//! Setup/hook pods and long-lived service pods, staged with zero supervisor lock held.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use slipfork_world::{BootFailure, BootState, BootStep, PodSpec, Stream};

use super::WorldRuntime;
use super::exit_line;
use super::helpers::{
    ServiceLaunch, dev_binds, merge_path, sealed_identity, shell_command, upsert_env,
};
use super::ready::{Ready, Staged};
use crate::world::supervise::BootSink;

impl WorldRuntime {
    pub(super) fn run_once(
        &self,
        step: &BootStep,
        log: &mut BootSink,
    ) -> Result<String, BootFailure> {
        if step.uses_env {
            return Err(BootFailure::with_remedy(
                "this step reads `$env.`, which this build cannot resolve",
                "replace `$env.<KEY>` with a literal value, or export the variable inside the step's own `run:` shell",
            ));
        }
        let command = step
            .command
            .clone()
            .ok_or_else(|| BootFailure::new(format!("`{}` has no command", step.label)))?;
        let built = self
            .built_for(step.image.as_ref(), log)
            .map_err(BootFailure::new)?;
        let identity = sealed_identity(&self.layout, &self.engine, &built).map_err(|error| {
            log.append_stream(Stream::Stderr, error.cause());
            log.append_stream(Stream::Stderr, error.remedy());
            BootFailure::with_remedy(error.cause(), error.remedy())
        })?;
        let dev = match step.image.as_deref() {
            Some(image) => self
                .dev_mounts
                .dev_for(image, &built)
                .map_err(BootFailure::new)?,
            None => None,
        };
        let rootfs = self
            .clone_rootfs(&built.rootfs, &format!("boot{}", step.line), false)
            .map_err(BootFailure::new)?;
        let seq = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let spec = PodSpec {
            id: format!("boot-{seq}"),
            rootfs,
            workdir: dev
                .as_ref()
                .map_or_else(|| built.workdir.clone(), |mount| mount.workdir.clone()),
            argv: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                shell_command(&command),
            ],
            inherited_env: built.inherited_env,
            env: self.pod_env_for(&step.subject, step.image.as_deref(), None),
            binds: dev_binds(&self.features, &self.project, dev.as_ref()),
            files: Vec::new(),
            identity: Some(identity),
        };
        // Setup and hook pods run to completion over `control.sock` with their pipes attached, so
        // independent nodes never stall behind a long `pre-deploy` seed.
        let mut tail: VecDeque<String> = VecDeque::new();
        let code = self
            .run_pod_once(&spec, &mut |stream, line| {
                log.append_stream(stream, line);
                if tail.len() == 10 {
                    tail.pop_front();
                }
                tail.push_back(line.to_owned());
            })
            .map_err(|error| BootFailure::new(error.to_string()))?;
        if code == 0 {
            Ok("ok".to_owned())
        } else {
            // A command that printed nothing still leaves one real line in its run capture, so a
            // failed step is never reported against an empty `slipfork logs -r <id>`.
            let line = exit_line(&command, code);
            log.append_stream(Stream::Stdout, &line);
            let mut message = line;
            for line in tail {
                message.push_str("\n  ");
                message.push_str(&line);
            }
            Err(BootFailure::new(message).with_exit_code(code))
        }
    }

    pub(super) fn start_service(
        &self,
        step: &BootStep,
        log: &mut BootSink,
    ) -> Result<Staged, BootFailure> {
        let sigma = step.subject.clone();
        let image = step.image.as_deref().ok_or_else(|| {
            BootFailure::with_remedy(
                format!("service `{sigma}` has no enclosing image"),
                "declare the service inside its image block",
            )
        })?;
        let built = self
            .built_for(step.image.as_ref(), log)
            .map_err(BootFailure::new)?;
        let identity = sealed_identity(&self.layout, &self.engine, &built).map_err(|error| {
            // An unresolvable `USER` must be visible before the pane closes: stream its cause and
            // concrete fix through the step's own gutter, and keep the fix as the envelope remedy.
            log.append_stream(Stream::Stderr, error.cause());
            log.append_stream(Stream::Stderr, error.remedy());
            BootFailure::with_remedy(error.cause(), error.remedy())
        })?;
        // Defense in depth: the same derivation that passed preflight refuses the bind here too.
        let dev = self
            .dev_mounts
            .dev_for(image, &built)
            .map_err(BootFailure::new)?;
        let rootfs = self
            .clone_rootfs(&built.rootfs, &format!("svc-{sigma}"), true)
            .map_err(BootFailure::new)?;
        let port = self
            .services
            .iter()
            .find(|service| service.id == sigma)
            .and_then(|service| service.port);
        // Build the merged pod env before resolving the launch so a `Forkfile`/image `PATH` is
        // the base the shim directory is prepended to — never something we overwrite.
        let env = self.pod_env_for(&step.subject, step.image.as_deref(), port);
        let path = merge_path(&env, &built.inherited_env);
        let mut launch_env = Vec::new();
        let (argv, display) = if let Some(command) = &step.command {
            let launch = ServiceLaunch::resolve(
                &self.project,
                dev.as_ref()
                    .map_or(built.workdir.as_str(), |mount| mount.workdir.as_str()),
                &rootfs,
                &path,
                command,
            );
            launch_env = launch.env;
            (launch.argv, command.clone())
        } else if built.argv.is_empty() {
            return Err(BootFailure::new(format!(
                "service `{sigma}` declares no `run:` and its image has no `ENTRYPOINT`/`CMD`"
            )));
        } else {
            (built.argv.clone(), built.argv.join(" "))
        };
        let mut spec = PodSpec {
            id: sigma,
            rootfs,
            workdir: dev
                .as_ref()
                .map_or_else(|| built.workdir.clone(), |mount| mount.workdir.clone()),
            argv,
            inherited_env: built.inherited_env,
            env,
            binds: dev_binds(&self.features, &self.project, dev.as_ref()),
            files: Vec::new(),
            identity: Some(identity),
        };
        upsert_env(&mut spec.env, launch_env);
        self.launch_pod(&spec).map_err(BootFailure::new)?;
        let ready = match port {
            Some(port) => Ready::Service {
                pods_dir: self.pods_dir.clone(),
                port,
                label: display,
                service: spec.id.clone(),
            },
            None => Ready::Portless {
                pods_dir: self.pods_dir.clone(),
                service: spec.id.clone(),
            },
        };
        Ok(Staged {
            state: BootState::Ready,
            detail: "started".to_owned(),
            ready,
            progress: None,
        })
    }
}
