//! Resolved world/component environments, ingress routes, forward targets, and hooks.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipfork_edge::forward::ForwardTargetSpec;
use slipfork_edge::{EdgeError, IngressRoute, world_id};
use slipfork_forkfile::{EvaluatedFork, HookPhase, Resolvers, TaintedString, WorldFact};
use slipfork_ospp::{OsppClaims, SubscriptionEnv};
use slipfork_wire::control::BootEvent;
use slipfork_world::{ExecEnv, InheritedVar};

use super::fork::fact_port;
use super::service::DaemonService;
use super::types::{HookLog, LiveWorld, ResolvedComponent, ResolvedHook};
use super::{WORLD_FEATURE, rejected, runtime_error};
use crate::layout::ensure_private_dir;
use crate::project::forkfile_error;

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
///
/// Every port comes from the canonical [`EvaluatedFork::world_facts`] projection, so a forwarded
/// `pg_databases.main` dials exactly the port `$app_db.port` publishes.
pub(super) fn forward_specs(
    fork: &EvaluatedFork,
    world: &str,
    components: &[ResolvedComponent],
    requested: &[String],
) -> Result<Vec<ForwardTargetSpec>, EdgeError> {
    let facts = fork.world_facts();
    let mut specs: Vec<ForwardTargetSpec> = Vec::new();
    for database in fork.pg_databases() {
        let id = format!("pg_databases.{}", database.database);
        specs.push(ForwardTargetSpec {
            port: required_port(&facts, &database.database, &id)?,
            scheme: "postgres".to_owned(),
            path: format!("/{}", database.database),
            id,
        });
    }
    for cluster in &fork.redis {
        let id = format!("redis_clusters.{}", cluster.id);
        specs.push(ForwardTargetSpec {
            port: required_port(&facts, &cluster.id, &id)?,
            scheme: "redis".to_owned(),
            path: String::new(),
            id,
        });
    }
    for bucket in &fork.s3 {
        let id = format!("s3_buckets.{bucket}");
        specs.push(ForwardTargetSpec {
            port: required_port(&facts, bucket, &id)?,
            scheme: "http".to_owned(),
            path: String::new(),
            id,
        });
    }
    for component in components {
        if let Some(port) = component.readiness_port {
            specs.push(ForwardTargetSpec {
                id: format!("components.{}", component.id),
                port,
                scheme: "http".to_owned(),
                path: String::new(),
            });
        }
    }
    if requested.is_empty() {
        if specs.is_empty() {
            return Err(rejected(format!(
                "world '{world}' has no forwardable target: declare a `postgres`, `redis`, or `s3` resource, or give a service an `internal_port`"
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

/// A projected port a world cannot run without: dialing `0` would silently forward nowhere.
fn required_port(facts: &[WorldFact], entity: &str, id: &str) -> Result<u16, EdgeError> {
    fact_port(facts, entity).ok_or_else(|| {
        rejected(format!(
            "Forkfile projects no `port` for '{id}'\n  next: declare `port <n>`"
        ))
    })
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

/// The `IngressRoute` prefix table derived from every service `route`.
pub(super) fn ingress_routes(fork: &EvaluatedFork) -> Result<Vec<IngressRoute>, EdgeError> {
    let mut routes: Vec<IngressRoute> = Vec::new();
    for route in fork.ingress_routes() {
        let Some(port) = route.port else {
            return Err(rejected(format!(
                "service '{}' declares `route {}` but no `port` to serve it\n  next: add `port <n>` to `service {}`",
                route.service, route.route, route.service
            )));
        };
        let port = u16::try_from(port).map_err(|_| {
            rejected(format!(
                "service '{}' declares port {port}, which is not a TCP port\n  next: declare a port between 1 and 65535",
                route.service
            ))
        })?;
        routes.push(IngressRoute {
            prefix: route.route,
            port,
        });
    }
    if routes.is_empty() {
        return Err(rejected(format!(
            "world has no HTTP ingress\n  next: add `route /` to a service in the Forkfile (feature '{WORLD_FEATURE}')"
        )));
    }
    Ok(routes)
}

/// Apply one already-resolved environment map to an [`ExecEnv`], taint-aware.
pub(super) fn apply_env(
    env: &mut ExecEnv,
    resolved: BTreeMap<String, TaintedString>,
) -> Result<(), EdgeError> {
    for (key, value) in resolved {
        let outcome = if value.is_tainted() {
            env.set_secret(&key, value.expose())
        } else {
            env.set(&key, value.expose())
        };
        outcome
            .map_err(|err| runtime_error(format!("invalid world environment '{key}': {err}")))?;
    }
    Ok(())
}

/// One world's environment resolver: the canonical `Forkfile` projection plus the OSPP layer.
///
/// Claimed keys are dropped from the server layer: the server echoes their canonical `Forkfile`
/// source, which the world must never receive as a literal. The local resolver fills them.
pub(super) struct WorldEnv<'a, 'r> {
    fork: &'a EvaluatedFork,
    resolvers: &'a Resolvers<'r>,
    claims: &'a OsppClaims,
    home: PathBuf,
    inherited: &'a [(InheritedVar, String)],
}

impl<'a, 'r> WorldEnv<'a, 'r> {
    pub(super) fn new(
        fork: &'a EvaluatedFork,
        resolvers: &'a Resolvers<'r>,
        claims: &'a OsppClaims,
        home: PathBuf,
        inherited: &'a [(InheritedVar, String)],
    ) -> Self {
        Self {
            fork,
            resolvers,
            claims,
            home,
            inherited,
        }
    }

    /// The world-scoped base environment: every `env` group plus the unclaimed server layer.
    pub(super) fn base(
        &self,
        server: Option<&BTreeMap<String, BTreeMap<String, TaintedString>>>,
    ) -> Result<ExecEnv, EdgeError> {
        let resolved = self
            .fork
            .world_env(self.resolvers)
            .map_err(forkfile_error)?;
        let mut env = ExecEnv::world_scoped(self.home.clone());
        for (var, value) in self.inherited {
            env.inherit(*var, value.clone());
        }
        apply_env(&mut env, resolved)?;
        let Some(components) = server else {
            return Ok(env);
        };
        let claimed = self.fork.all_claimed_env_keys(&self.owns());
        for component in components.values() {
            overlay(&mut env, component, &claimed)?;
        }
        Ok(env)
    }

    /// One component's environment: its own assignments plus only its own server layer.
    pub(super) fn component(
        &self,
        id: &str,
        server: Option<&BTreeMap<String, TaintedString>>,
    ) -> Result<ExecEnv, EdgeError> {
        let resolved = self
            .fork
            .resolve_env(id, self.resolvers)
            .map_err(forkfile_error)?;
        let claimed = self.fork.claimed_env_keys(id, &self.owns());
        let mut env = ExecEnv::default();
        apply_env(&mut env, resolved)?;
        if let Some(server) = server {
            overlay(&mut env, server, &claimed)?;
        }
        Ok(env)
    }

    fn owns(&self) -> impl Fn(&str, &[String]) -> bool + '_ {
        |namespace, path| self.claims.owns(namespace, path)
    }
}

/// Layer one server-resolved environment on top, skipping every locally claimed key.
fn overlay(
    env: &mut ExecEnv,
    server: &BTreeMap<String, TaintedString>,
    claimed: &BTreeSet<String>,
) -> Result<(), EdgeError> {
    for (name, value) in server {
        if claimed.contains(name) {
            continue;
        }
        let outcome = if value.is_tainted() {
            env.set_secret(name, value.expose())
        } else {
            env.set(name, value.expose())
        };
        outcome
            .map_err(|err| runtime_error(format!("invalid OSPP environment '{name}': {err}")))?;
    }
    Ok(())
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
        HookPhase::Setup => "setup",
        HookPhase::PreDeploy => "pre-deploy",
        HookPhase::PostDeploy => "post-deploy",
        HookPhase::FailedDeploy => "failed-deploy",
    }
}

/// Look one already-resolved `<service>:<phase>` hook up by the id the CLI names.
pub(super) fn resolve_hook<'a>(
    hooks: &'a [ResolvedHook],
    hook_id: &str,
) -> Result<&'a ResolvedHook, EdgeError> {
    hooks.iter().find(|hook| hook.id == hook_id).ok_or_else(|| {
        let known = hooks
            .iter()
            .map(|hook| hook.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        rejected(format!(
            "world has no hook '{hook_id}' (declared hooks: {})\n  next: name one of the declared hooks, or declare one as `<phase>: <command>` in the service",
            if known.is_empty() { "none" } else { &known }
        ))
    })
}

impl DaemonService {
    /// Run one deploy phase's hooks, returning how many ran.
    pub(super) fn run_hooks(
        &self,
        live: &mut LiveWorld,
        phase: HookPhase,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<usize, EdgeError> {
        let hooks: Vec<ResolvedHook> = live
            .hooks
            .iter()
            .filter(|hook| hook.phase == phase)
            .cloned()
            .collect();
        let mut ran = 0;
        for hook in hooks {
            let id = hook.id.clone();
            on_event(BootEvent::Hook {
                id: id.clone(),
                argv: hook.argv.clone(),
            });
            let log = self.hook_log(live, &id, hook_phase_label(hook.phase))?;
            let mut file = open_hook_log(&log.path)?;
            let output = self
                .engine
                .exec_in_world(
                    &live.handle,
                    &hook.argv,
                    &live.worktree,
                    &hook.overlay,
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
                    argv: hook.argv,
                    exit_code: output.exit_code,
                    world: world.clone(),
                    log_path: log.path.clone(),
                });
                return Err(runtime_error(format!(
                    "hook '{id}' exited with code {} in world '{world}' (the world is kept for inspection; full output is in {})\n  next: fix the hook, then run `slipfork restart`",
                    output.exit_code,
                    log.path.display()
                )));
            }
            ran += 1;
        }
        Ok(ran)
    }
}

impl DaemonService {
    /// Apply one already-decrypted `env_updated` environment to this daemon's live worlds:
    /// recompute each component's environment from the local vault + projection, and restart
    /// **only** the components whose environment actually changed.
    pub(super) fn apply_env_update(&self, environment: &SubscriptionEnv) -> Result<(), EdgeError> {
        let Some(project) = self.ospp.project_for(&environment.id) else {
            return Err(runtime_error(format!(
                "OSPP pushed an environment for unknown subscription '{}'",
                environment.id
            )));
        };
        let _guard = self.boot_guard();
        let mut state = self.lock();
        let provider = self.vault_for(&project)?;
        let resolvers = Resolvers::new(provider.as_ref(), self.entropy.as_ref());
        for world in state
            .worlds
            .iter_mut()
            .filter(|world| world.project == project)
        {
            let claims = self.ospp.claims_for(
                &project,
                &world.fork,
                &world.plan,
                self.vault_store(&project)?,
            )?;
            let home = self
                .config
                .layout
                .world_home(&project, &world_id(&world.project, &world.target));
            let resolved = WorldEnv::new(
                &world.fork,
                &resolvers,
                &claims,
                home,
                &self.config.inherited,
            );
            let mut updated = Vec::with_capacity(world.components.len());
            for component in &world.components {
                let env =
                    resolved.component(&component.id, environment.component(&component.id))?;
                updated.push(env);
            }
            for (component, env) in world.components.iter_mut().zip(updated) {
                if env == component.env {
                    continue;
                }
                let handle = world
                    .services
                    .iter()
                    .find(|service| service.id() == component.id)
                    .cloned();
                if let Some(handle) = handle {
                    self.engine.update_service_env(&handle, &env).map_err(|err| {
                        runtime_error(format!(
                            "cannot restart component '{}' with the pushed OSPP environment: {err}",
                            component.id
                        ))
                    })?;
                }
                component.env = env;
            }
            world.ospp_env = Some(environment.components.clone());
        }
        Ok(())
    }
}
