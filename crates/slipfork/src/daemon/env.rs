//! Resolved world/component environments, ingress routes, forward targets, and hooks.
use slipfork_wire::control::BootEvent;
use std::io::Write;

use super::service::DaemonService;
use super::types::{HookLog, LiveWorld, ResolvedComponent, ResolvedHook};
use super::{WORLD_MODE, rejected, runtime_error, snow_error};
use crate::layout::ensure_private_dir;
use slipfork_edge::forward::ForwardTargetSpec;
use slipfork_edge::{EdgeError, IngressRoute, world_id};
use slipfork_ospp::OsppClaims;
use slipfork_snow::{
    ClaimedEvaluator, EntropySource, HookPhase, ResolvedEnvMap, ResourceCoordinates,
    SecretProvider, SecretValue, SnowFile,
};
use slipfork_world::{ExecEnv, InheritedVar};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;

impl DaemonService {
    /// The retained log for one hook, created once per world and reused by `hook run`.
    pub(super) fn hook_log(
        &self,
        live: &mut LiveWorld,
        id: &str,
        phase: &str,
    ) -> Result<Arc<HookLog>, EdgeError> {
        if let Some(log) = live.hook_logs.get(id) {
            return Ok(Arc::clone(log));
        }
        let directory = self
            .config
            .layout
            .world_logs_dir(&live.project, &world_id(&live.project, &live.target));
        ensure_private_dir(&directory).map_err(runtime_error)?;
        let (component, _) = id
            .rsplit_once(':')
            .ok_or_else(|| rejected(format!("invalid hook id '{id}'")))?;
        let log = Arc::new(HookLog::new(
            id.to_owned(),
            directory.join(format!("{component}.{phase}.log")),
        ));
        live.hook_logs.insert(id.to_owned(), Arc::clone(&log));
        Ok(log)
    }
}

/// Open (truncate) one hook's `0600` retained log for a fresh run.
pub(super) fn open_hook_log(path: &Path) -> Result<File, EdgeError> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| runtime_error(format!("cannot write {}: {err}", path.display())))
}

/// The `forward` targets a world exposes: declared resources plus enabled component ports.
pub(super) fn forward_specs(
    snow: &SnowFile,
    world: &str,
    components: &[ResolvedComponent],
    requested: &[String],
) -> Result<Vec<ForwardTargetSpec>, EdgeError> {
    let mut specs: Vec<ForwardTargetSpec> = Vec::new();
    for db in &snow.resources.pg_databases {
        specs.push(ForwardTargetSpec {
            id: format!("pg_databases.{}", db.id),
            port: 5432,
            scheme: "postgres".to_owned(),
            path: format!("/{}", db.id),
        });
    }
    for cluster in &snow.resources.redis_clusters {
        specs.push(ForwardTargetSpec {
            id: format!("redis_clusters.{}", cluster.id),
            port: 6379,
            scheme: "redis".to_owned(),
            path: String::new(),
        });
    }
    for bucket in &snow.resources.s3_buckets {
        specs.push(ForwardTargetSpec {
            id: format!("s3_buckets.{}", bucket.id),
            // `ADR-0009` §2 pins the SeaweedFS container to `-port=9000` on the world loopback;
            // `snow.yml` carries no per-bucket S3 port to read.
            port: 9000,
            scheme: "http".to_owned(),
            path: String::new(),
        });
    }
    for component in &snow.components {
        if !components.iter().any(|enabled| enabled.id == component.id) {
            continue;
        }
        if let Some(port) = component.internal_ports.first() {
            specs.push(ForwardTargetSpec {
                id: format!("components.{}", component.id),
                port: *port,
                scheme: "http".to_owned(),
                path: String::new(),
            });
        }
    }
    if requested.is_empty() {
        if specs.is_empty() {
            return Err(rejected(format!(
                "world '{world}' has no forwardable target: declare snow.yml resources or component internal_ports"
            )));
        }
        return Ok(specs);
    }
    let mut selected = Vec::with_capacity(requested.len());
    for id in requested {
        let spec = specs.iter().find(|spec| spec.id == *id).ok_or_else(|| {
            let known = specs
                .iter()
                .map(|spec| spec.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            rejected(format!(
                "world '{world}' has no forward target '{id}' (known targets: {known})"
            ))
        })?;
        if !selected
            .iter()
            .any(|existing: &ForwardTargetSpec| existing.id == *id)
        {
            selected.push(spec.clone());
        }
    }
    Ok(selected)
}

pub(super) fn container_env(
    home: &str,
    vars: &[(&str, &str)],
    path: &str,
) -> Result<ExecEnv, EdgeError> {
    let mut env = ExecEnv::world_scoped(home);
    env.inherit(InheritedVar::Path, path);
    // `LANG` is a closed inherited variable, so an OCI image without the host locale gets `C`.
    env.inherit(
        InheritedVar::Lang,
        if home == "/data" { "C" } else { "C.UTF-8" },
    );
    for (name, value) in vars {
        env.set(name, *value)
            .map_err(|err| runtime_error(err.to_string()))?;
    }
    Ok(env)
}

/// Build the `IngressRoute` prefix table: declared `snow.yml` ingresses plus the canonical
/// empty-prefix catch-all from the first enabled component's first `internal_ports` entry.
pub(super) fn ingress_routes(
    snow: &SnowFile,
    effective: &[(String, slipfork_snow::ResolvedComponent)],
) -> Result<Vec<IngressRoute>, EdgeError> {
    let mut routes: Vec<IngressRoute> = snow
        .ingresses
        .iter()
        .map(|ingress| IngressRoute {
            prefix: ingress.match_rule.prefix.clone(),
            port: ingress.target.port,
        })
        .collect();
    if !routes.iter().any(|route| route.prefix.is_empty()) {
        for (id, resolved) in effective {
            if resolved.disabled {
                continue;
            }
            if let Some(port) = snow
                .components
                .iter()
                .find(|component| component.id == *id)
                .and_then(|component| component.internal_ports.first())
            {
                routes.push(IngressRoute {
                    prefix: String::new(),
                    port: *port,
                });
                break;
            }
        }
    }
    if routes.is_empty() {
        return Err(rejected(format!(
            "world has no HTTP ingress: declare `ingresses:` in snow.yml or give a component an \
             `internal_ports` entry (mode '{WORLD_MODE}')"
        )));
    }
    Ok(routes)
}

pub(super) fn apply_env(env: &mut ExecEnv, resolved: ResolvedEnvMap) -> Result<(), EdgeError> {
    for (key, value) in resolved {
        let outcome = if value.is_secret() {
            env.set_secret(&key, value.expose())
        } else {
            env.set(&key, value.expose())
        };
        outcome
            .map_err(|err| runtime_error(format!("invalid world environment '{key}': {err}")))?;
    }
    Ok(())
}

/// One component's environment: local `snow.yml` resolution first, then the server-resolved
/// values, with only **claimed** `${{ ... }}` expressions substituted locally.
pub(super) fn resolve_component_env(
    snow: &SnowFile,
    provider: &dyn SecretProvider,
    coords: &ResourceCoordinates,
    component: &str,
    claims: &OsppClaims,
    server: Option<&BTreeMap<String, SecretValue>>,
    entropy: &dyn EntropySource,
) -> Result<ExecEnv, EdgeError> {
    let owns = |namespace: &str, path: &[String]| claims.owns(namespace, path);
    let evaluator = ClaimedEvaluator::new(provider, coords, entropy, &owns);
    let resolved = evaluator
        .mode(snow, WORLD_MODE, Some(component))
        .map_err(snow_error)?;
    let mut env = ExecEnv::default();
    apply_env(&mut env, resolved)?;
    let Some(server) = server else {
        return Ok(env);
    };
    for (name, value) in server {
        let rendered = evaluator.render(value.expose()).map_err(snow_error)?;
        let secret = value.is_secret() || rendered.is_secret();
        let raw = rendered.expose().to_owned();
        let outcome = if secret {
            env.set_secret(name, raw)
        } else {
            env.set(name, raw)
        };
        outcome
            .map_err(|err| runtime_error(format!("invalid OSPP environment '{name}': {err}")))?;
    }
    Ok(env)
}

pub(super) fn validate_secret_key(key: &str) -> Result<(), EdgeError> {
    if key.is_empty()
        || key
            .chars()
            .any(|character| character.is_control() || character == '=')
    {
        return Err(rejected(format!(
            "invalid secret key '{key}': keys are non-empty printable names without '='"
        )));
    }
    Ok(())
}

pub(super) fn hook_phase_label(phase: HookPhase) -> &'static str {
    match phase {
        HookPhase::PreDeploy => "pre-deploy",
        HookPhase::PostDeploy => "post-deploy",
        HookPhase::FailedDeploy => "failed-deploy",
    }
}

/// Resolve one `<component>:<phase>` hook through the single validation path used by both
/// interactive `hook run` (entry argv) and `--no-tty` streaming.
pub(super) fn resolve_hook(
    snow: &SnowFile,
    components: &[ResolvedComponent],
    hook_id: &str,
) -> Result<ResolvedHook, EdgeError> {
    let (component, phase) = parse_hook_id(hook_id).ok_or_else(|| {
        rejected(format!(
            "invalid hook id '{hook_id}' (expected <component>:<phase>)"
        ))
    })?;
    let Some(enabled) = components
        .iter()
        .find(|candidate| candidate.id == component)
    else {
        return Err(rejected(format!(
            "component '{component}' is not enabled in mode '{WORLD_MODE}'"
        )));
    };
    let hook = snow
        .hooks
        .iter()
        .find(|hook| hook.component == component && hook.when == phase)
        .ok_or_else(|| rejected(format!("world has no hook '{hook_id}'")))?;
    if hook.run_command.trim().is_empty() {
        return Err(rejected(format!(
            "hook '{hook_id}' declares an empty run_command"
        )));
    }
    Ok(ResolvedHook {
        argv: vec![
            "sh".to_owned(),
            "-eu".to_owned(),
            "-c".to_owned(),
            hook.run_command.clone(),
        ],
        overlay: enabled.env.clone(),
        phase_label: hook_phase_label(phase),
    })
}

pub(super) fn parse_hook_id(hook: &str) -> Option<(&str, HookPhase)> {
    let (component, phase) = hook.rsplit_once(':')?;
    let phase = match phase {
        "pre-deploy" => HookPhase::PreDeploy,
        "post-deploy" => HookPhase::PostDeploy,
        "failed-deploy" => HookPhase::FailedDeploy,
        _ => return None,
    };
    (!component.is_empty()).then_some((component, phase))
}

impl DaemonService {
    /// Run one deploy phase's hooks, returning how many ran.
    pub(super) fn run_hooks(
        &self,
        live: &mut LiveWorld,
        phase: HookPhase,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<usize, EdgeError> {
        let hooks = live.snow.hooks.clone();
        let mut ran = 0;
        for hook in hooks {
            let Some(component) = live
                .components
                .iter()
                .find(|component| component.id == hook.component)
            else {
                continue;
            };
            if hook.when != phase {
                continue;
            }
            if hook.run_command.trim().is_empty() {
                return Err(rejected(format!(
                    "hook '{}:{}' declares an empty run_command",
                    hook.component,
                    hook_phase_label(hook.when)
                )));
            }
            let id = format!("{}:{}", hook.component, hook_phase_label(hook.when));
            let argv = vec![
                "sh".to_owned(),
                "-eu".to_owned(),
                "-c".to_owned(),
                hook.run_command.clone(),
            ];
            on_event(BootEvent::Hook {
                id: id.clone(),
                argv: argv.clone(),
            });
            let overlay = component.env.clone();
            let log = self.hook_log(live, &id, hook_phase_label(hook.when))?;
            let mut file = open_hook_log(&log.path)?;
            let output = self
                .engine
                .exec_in_world(
                    &live.handle,
                    &argv,
                    &live.worktree,
                    &overlay,
                    &mut |chunk| {
                        let _ = file.write_all(&chunk.bytes);
                        let _ = file.flush();
                        log.push(chunk.clone());
                        on_event(BootEvent::HookOutput {
                            id: id.clone(),
                            chunk,
                        });
                    },
                )
                .map_err(|err| {
                    log.close();
                    runtime_error(format!(
                        "cannot run hook '{id}' in world '{}': {err}",
                        world_id(&live.project, &live.target)
                    ))
                })?;
            log.close();
            if output.exit_code != 0 {
                let world = world_id(&live.project, &live.target);
                on_event(BootEvent::HookFailed {
                    id: id.clone(),
                    argv,
                    exit_code: output.exit_code,
                    world: world.clone(),
                    log_path: log.path.clone(),
                });
                return Err(runtime_error(format!(
                    "hook '{id}' exited with code {} in world '{world}' (the world is kept for inspection; full output is in {})",
                    output.exit_code,
                    log.path.display()
                )));
            }
            ran += 1;
        }
        Ok(ran)
    }
}
