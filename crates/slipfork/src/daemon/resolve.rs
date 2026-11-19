//! The one pre-world resolver: manifest projection, resources, environments, rootfs, and base.

use super::backing::requested_service_images;
use super::env::{WorldEnv, hook_phase_label, ingress_routes};
use super::fork::load_project_fork;
use super::rootfs_select::select_single_build_image;
use super::service::DaemonService;
use super::types::{
    BootPolicy, PreparedWorld, ResolvedComponent, ResolvedHook, ResolvedWorld, RootfsSource,
};
use super::{rejected, runtime_error};
use crate::project::forkfile_error;
use slipfork_dag::{DumpPolicy, ResourceMode, ResourcePlan, ResourceStrategy, ResourcesSpec};
use slipfork_edge::{EdgeError, world_id};
use slipfork_engine::rootfs::SnowPortContract;
use slipfork_engine::rootfs::WorldRootfsRequest;
use slipfork_engine::rootfs::{HostCacheRequest, RootfsBuildEvent};
use slipfork_forkfile::{EvaluatedFork, EvaluatedService, HookPhase, Resolvers};
use slipfork_vcs::RepoRoot;
use slipfork_wire::control::{BootEvent, ResourceSummary};
use slipfork_world::WorldMount;
use slipfork_world::rootfs::{BuildEnvVar, WorldRootfs};
use slipfork_world::{
    ExecEnv, LocalHostRoute, LocalIngressBridge, MountMode, WorldSpec, WorldTarget,
};

impl DaemonService {
    /// Materialize (or reuse) the selected `Forkfile` `Dockerfile` service as the world's `L0`
    /// rootfs (`ADR-0009` §1–§3), selecting its one buildable image and every backing service.
    pub(super) fn materialize_rootfs(
        &self,
        source: RootfsSource<'_>,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<WorldRootfs, EdgeError> {
        let RootfsSource {
            worktree,
            fork,
            resolvers,
            host_caches,
        } = source;
        let images = fork.build_images();
        let ids: Vec<String> = images.iter().map(|image| image.service.clone()).collect();
        // Per-service pods read their image from `Forkfile`; the transitional single-rootfs boot
        // can only materialize a project that declares exactly one buildable Dockerfile.
        let image = &images[select_single_build_image(&ids)?];
        let context_dir = worktree.join(&image.context_dir);
        let dockerfile = context_dir.join(&image.dockerfile);

        let resolved = fork
            .resolve_build_env(&image.service, resolvers)
            .map_err(forkfile_error)?;
        let build_envs: Vec<BuildEnvVar> = resolved
            .into_iter()
            .map(|(name, value)| BuildEnvVar {
                name,
                value: value.expose().to_owned(),
            })
            .collect();

        let service = fork
            .services
            .iter()
            .find(|service| service.id == image.service);
        let ports = SnowPortContract {
            component: image.service.clone(),
            internal_ports: match service {
                Some(service) => internal_ports(service)?,
                None => Vec::new(),
            },
            ingress_ports: fork
                .ingress_routes()
                .into_iter()
                .filter(|route| route.service == image.service)
                .filter_map(|route| route.port)
                .map(|port| tcp_port(&image.service, port))
                .collect::<Result<Vec<_>, _>>()?,
        };

        let request = WorldRootfsRequest {
            state_dir: self.config.layout.state_dir().to_path_buf(),
            image_id: image.service.clone(),
            context_dir,
            context_label: image.context_dir.clone(),
            dockerfile,
            target_stage: image.stage.clone(),
            build_envs,
            target_arch: self.config.target_arch.clone(),
            services: requested_service_images(fork),
            ports,
            uid: self.config.uid,
            gid: self.config.gid,
            runtime: self.config.runtime.clone(),
            host_caches: if host_caches {
                HostCacheRequest {
                    host_shell: self.config.host_caches.witness.clone(),
                    mounts: self.config.host_caches.mounts.clone(),
                }
            } else {
                HostCacheRequest::default()
            },
        };
        self.builder
            .materialize(&request, &mut |event: RootfsBuildEvent| {
                // One live line of buildx/pull/export progress, numbered by the engine's own
                // rootfs pipeline; cache hit/miss stays silent.
                if let RootfsBuildEvent::Progress {
                    step,
                    total,
                    phase,
                    detail,
                } = event
                {
                    on_event(BootEvent::Progress {
                        step,
                        total,
                        phase,
                        detail,
                    });
                }
            })
            .map_err(|err| runtime_error(err.to_string()))
    }
}

impl DaemonService {
    /// Resolve every `Forkfile`- and flag-derived fact one pipeline run needs, before anything is
    /// materialized or started.
    ///
    /// `boot` and `reboot` call this same resolver, so a `restart` re-derives the resource plan,
    /// environments, enabled components, and ingress/domain table from the edited manifest
    /// instead of reusing boot-time values.
    pub(super) fn resolve_world(
        &self,
        repo: &RepoRoot,
        target: &WorldTarget,
        worktree: &std::path::Path,
        policy: BootPolicy,
        feature_selection: &slipfork_wire::control::FeatureSelection,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<ResolvedWorld, EdgeError> {
        let project = &repo.project;
        let fork = load_project_fork(worktree, feature_selection)?;
        let vault = self.vault_for(project)?;
        let resolvers = Resolvers::new(vault.as_ref(), self.entropy.as_ref());

        let ingresses = ingress_routes(&fork)?;
        let domains = fork.domains.clone();
        let domain_refs: Vec<&str> = domains.iter().map(String::as_str).collect();
        // Validate the world's `.localhost` route before anything is materialized; the origin
        // itself stays derivable on demand from the registry.
        let route = LocalHostRoute::new(project.clone(), target.clone(), None, &domain_refs)
            .map_err(|err| runtime_error(err.to_string()))?;
        self.registry.mint_origin(&route, self.ingress.port())?;

        // OSPP project identity is the verbatim Git remote and only that string: without an
        // origin there is nothing to subscribe, even when the machine key is paired.
        let remote = self.repo_roots.remote(repo).map_err(|err| {
            runtime_error(format!(
                "cannot read the repository's Git origin for OSPP: {err}"
            ))
        })?;
        // One live session per project while worlds run; the plan only needs to know whether a
        // usable (paired and remote-identified) session exists. OSPP is never a prerequisite.
        let session = self.ospp.session()?;
        let ospp_paired = session.is_some() && remote.is_some();
        let plan = resolve_plan(
            &fork,
            target,
            policy.dump,
            policy.force_local_resources,
            ospp_paired,
        )?;
        let resources = plan_summaries(&plan);
        for resource in &resources {
            on_event(BootEvent::Resources {
                resource: resource.clone(),
            });
        }
        let claims = self
            .ospp
            .claims_for(project, &fork, &plan, self.vault_store(project)?)?;
        let ospp_env = match (session.as_ref(), remote.as_deref()) {
            (Some(_), Some(remote)) => self.ospp.subscribe(project, remote, &fork, &claims)?,
            _ => None,
        };
        let home = self
            .config
            .layout
            .world_home(project, &world_id(project, target));
        let world_env = WorldEnv::new(&fork, &resolvers, &claims, home, &self.config.inherited);
        let base_env = world_env.base(ospp_env.as_ref().map(|env| &env.components))?;
        let mut components = Vec::new();
        for service in &fork.services {
            let declared = internal_ports(service)?;
            let readiness_port = match declared.first() {
                Some(port) => Some(*port),
                None => service
                    .port
                    .map(|port| tcp_port(&service.id, port))
                    .transpose()?,
            };
            let env = world_env.component(
                &service.id,
                ospp_env.as_ref().and_then(|env| env.component(&service.id)),
            )?;
            components.push(ResolvedComponent {
                id: service.id.clone(),
                argv: component_argv(&fork, service, &resolvers)?,
                readiness_port,
                env,
            });
        }
        let hooks = resolve_hooks(&fork, &resolvers)?;
        Ok(ResolvedWorld {
            fork,
            vault,
            plan,
            resources,
            base_env,
            components,
            hooks,
            ingresses,
            domains,
            ospp_env: ospp_env.map(|environment| environment.components),
        })
    }
}

/// One declared service port as a real TCP port: a non-TCP value is a manifest refusal.
fn tcp_port(service: &str, port: u32) -> Result<u16, EdgeError> {
    u16::try_from(port).map_err(|_| {
        rejected(format!(
            "service '{service}' declares port {port}, which is not a TCP port\n  next: declare a port between 1 and 65535"
        ))
    })
}

/// Every declared `internal_port` of one service, in declaration order (none is skipped).
fn internal_ports(service: &EvaluatedService) -> Result<Vec<u16>, EdgeError> {
    service
        .internal_ports
        .iter()
        .map(|port| tcp_port(&service.id, *port))
        .collect()
}

/// One component's `run` argv through the canonical evaluator (never a shell re-parse).
fn component_argv(
    fork: &EvaluatedFork,
    service: &EvaluatedService,
    resolvers: &Resolvers<'_>,
) -> Result<Vec<String>, EdgeError> {
    let Some(run) = &service.run else {
        return Err(rejected(format!(
            "component '{}' declares no `run` command in the Forkfile\n  next: add `run <command>` to `service {}`",
            service.id, service.id
        )));
    };
    Ok(fork
        .resolve_argv(&service.id, run, resolvers)
        .map_err(forkfile_error)?
        .into_iter()
        .map(|argument| argument.expose().to_owned())
        .collect())
}

/// The `<service>:<phase>` id `slipfork hook run` names.
fn hook_id(service: &str, phase: HookPhase) -> String {
    format!("{service}:{}", hook_phase_label(phase))
}

/// Every declared lifecycle hook, resolved once through the canonical evaluator.
fn resolve_hooks(
    fork: &EvaluatedFork,
    resolvers: &Resolvers<'_>,
) -> Result<Vec<ResolvedHook>, EdgeError> {
    let mut hooks = Vec::new();
    for service in &fork.services {
        for hook in &service.hooks {
            let argv: Vec<String> = fork
                .resolve_argv(&service.id, &hook.argv, resolvers)
                .map_err(forkfile_error)?
                .into_iter()
                .map(|argument| argument.expose().to_owned())
                .collect();
            hooks.push(ResolvedHook {
                id: hook_id(&service.id, hook.phase),
                phase: hook.phase,
                argv,
                overlay: ExecEnv::default(),
            });
        }
    }
    Ok(hooks)
}

impl DaemonService {
    /// Resolve one run's world inputs and materialize the content-addressed `L0` rootfs and base
    /// snapshot — the one prep path `boot` and `reboot` share.
    ///
    /// The rootfs digest is content-addressed over the whole build context, so an edited
    /// `Dockerfile` or copied input yields a new digest here and the builder rebuilds it; an
    /// unchanged tree is served from the cache. Neither verb can skip this and boot a stale tree.
    pub(super) fn prepare_world(
        &self,
        repo: &RepoRoot,
        target: &WorldTarget,
        worktree: &std::path::Path,
        policy: BootPolicy,
        feature_selection: &slipfork_wire::control::FeatureSelection,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<PreparedWorld, EdgeError> {
        let resolved =
            self.resolve_world(repo, target, worktree, policy, feature_selection, on_event)?;
        let resolvers = Resolvers::new(resolved.vault.as_ref(), self.entropy.as_ref());
        let rootfs = self.materialize_rootfs(
            RootfsSource {
                worktree,
                fork: &resolved.fork,
                resolvers: &resolvers,
                host_caches: policy.host_caches,
            },
            on_event,
        )?;
        let (snapshot, boot) =
            self.ensure_base_snapshot(&repo.project, &resolved.fork, &resolved.plan, &rootfs)?;
        Ok(PreparedWorld {
            resolved,
            rootfs,
            snapshot,
            boot,
        })
    }
}

impl DaemonService {
    /// The one `WorldSpec` both verbs hand the engine: the worktree bind mount, the world-scoped
    /// environment, and the daemon's ingress bridge.
    pub(super) fn world_spec(
        &self,
        target: &WorldTarget,
        worktree: &std::path::Path,
        prepared: &PreparedWorld,
    ) -> WorldSpec {
        WorldSpec {
            target: target.clone(),
            rootfs: prepared.rootfs.clone(),
            snapshot: prepared.snapshot.clone(),
            mounts: vec![WorldMount {
                host_path: worktree.to_path_buf(),
                guest_path: worktree.to_path_buf(),
                mode: MountMode::ReadWrite,
            }],
            base_env: prepared.resolved.base_env.clone(),
            local_ingress: Some(LocalIngressBridge {
                port: self.ingress.port(),
                host_ingress_sock: self.config.layout.http_ingress_socket(),
            }),
            boot: prepared.boot.clone(),
        }
    }
}

pub(super) fn dump_policy(request: slipfork_wire::control::DumpRequest) -> DumpPolicy {
    match request {
        slipfork_wire::control::DumpRequest::Default => DumpPolicy::Default,
        slipfork_wire::control::DumpRequest::NoDump => DumpPolicy::NoDump,
        slipfork_wire::control::DumpRequest::RefreshDump => DumpPolicy::RefreshDump,
    }
}

/// Resolve one run's resource plan from the projection's declared ids and the boot policy.
pub(super) fn resolve_plan(
    fork: &EvaluatedFork,
    target: &WorldTarget,
    dump: DumpPolicy,
    force_local: bool,
    ospp_paired: bool,
) -> Result<ResourcePlan, EdgeError> {
    let databases = fork.pg_databases();
    let s3_ids: Vec<&str> = fork.s3.iter().map(String::as_str).collect();
    let pg_ids: Vec<&str> = databases
        .iter()
        .map(|database| database.database.as_str())
        .collect();
    let spec = ResourcesSpec::default().with_declared_ids(&s3_ids, &pg_ids);

    // `--resources local` is a per-invocation override (`ADR-0008` §3): the typed forced-strategy
    // spec keeps `ResourcePlan::resolve` the sole decision point while no seed dumps are used and
    // no OSPP offer is consulted.
    let (spec, dump) = if force_local {
        if dump == DumpPolicy::RefreshDump {
            return Err(rejected(
                "`--resources local` cannot be combined with `--refresh-dump`",
            ));
        }
        (
            spec.forced_strategy(ResourceStrategy::Local),
            DumpPolicy::NoDump,
        )
    } else {
        (spec, dump)
    };
    ResourcePlan::resolve(&spec, ResourceMode::from(target), dump, ospp_paired)
        .map_err(|err| runtime_error(err.to_string()))
}

pub(super) fn plan_summaries(plan: &ResourcePlan) -> Vec<ResourceSummary> {
    plan.diagnostics()
        .into_iter()
        .map(|diagnostic| ResourceSummary {
            id: diagnostic.id,
            strategy: super::types::json_label(&diagnostic.strategy),
            resolved: super::types::json_label(&diagnostic.resolved),
            reason: super::types::json_label(&diagnostic.reason),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipfork_wire::control::{DaemonError, DaemonErrorKind};

    /// Assert one typed refusal and return it (every refusal is a `next:`-carrying rejection).
    fn refused<T>(result: Result<T, EdgeError>) -> DaemonError {
        let error = match result {
            Ok(_) => panic!("expected a manifest refusal"),
            Err(EdgeError::Control(error)) => error,
            Err(other) => panic!("a refusal must be a typed control error, got {other:?}"),
        };
        assert_eq!(error.kind, DaemonErrorKind::Rejected, "got {error:?}");
        assert!(
            error.message.contains("\n  next: "),
            "one line plus the remedy: {}",
            error.message
        );
        error
    }

    #[test]
    fn every_hook_phase_round_trips_into_its_cli_id() {
        for (phase, label) in [
            (HookPhase::Setup, "setup"),
            (HookPhase::PreDeploy, "pre-deploy"),
            (HookPhase::PostDeploy, "post-deploy"),
            (HookPhase::FailedDeploy, "failed-deploy"),
        ] {
            assert_eq!(hook_id("api", phase), format!("api:{label}"));
        }
    }

    #[test]
    fn an_unknown_hook_id_fails_closed_with_the_declared_list() {
        let hooks = [ResolvedHook {
            id: hook_id("api", HookPhase::Setup),
            phase: HookPhase::Setup,
            argv: vec!["/bin/true".to_owned()],
            overlay: ExecEnv::default(),
        }];
        let error = refused(super::super::env::resolve_hook(&hooks, "api:unknown"));
        assert!(
            error.message.contains("world has no hook 'api:unknown'"),
            "{}",
            error.message
        );
        assert!(error.message.contains("api:setup"), "{}", error.message);
    }
}
