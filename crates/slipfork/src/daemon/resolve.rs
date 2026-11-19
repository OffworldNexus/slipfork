use super::backing::requested_service_images;
use super::env::{apply_env, ingress_routes, resolve_component_env};
use super::service::DaemonService;
use super::types::ResolvedComponent;
use super::types::{
    BootPolicy, PreparedWorld, ResolvedWorld, RootfsInputs, WorldEnvInputs, json_label, parent_dir,
};
use super::{WORLD_MODE, rejected, runtime_error, snow_error};
use crate::project::load_optional_slipfork_config;
use slipfork_dag::{DumpPolicy, ResourceMode, ResourcePlan, ResourceStrategy, SlipforkConfig};
use slipfork_edge::{EdgeError, world_id};
use slipfork_engine::rootfs::SnowPortContract;
use slipfork_engine::rootfs::WorldRootfsRequest;
use slipfork_engine::rootfs::{HostCacheRequest, RootfsBuildEvent};
use slipfork_snow::{ClaimedEvaluator, Image, OsCsprngEntropy, ResourceCoordinates, SnowFile};
use slipfork_vcs::RepoRoot;
use slipfork_wire::control::{BootEvent, ResourceSummary, SnowResolveRequest, SnowResolveSummary};
use slipfork_world::LocalIngressBridge;
use slipfork_world::WorldMount;
use slipfork_world::rootfs::{BuildEnvVar, WorldRootfs};
use slipfork_world::{ExecEnv, LocalHostRoute, MountMode, WorldSpec, WorldTarget};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

impl DaemonService {
    /// Load `snow.yml` and keep its raw bytes: OSPP `subscribe` sends the file verbatim.
    pub(super) fn load_snow(&self, worktree: &Path) -> Result<(SnowFile, String), EdgeError> {
        let path = worktree.join("snow.yml");
        let raw = fs::read_to_string(&path).map_err(|err| {
            runtime_error(format!(
                "cannot read {}: {err}; every world boot needs the project's snow.yml",
                path.display()
            ))
        })?;
        let parsed = SnowFile::parse_yaml(&raw)
            .map_err(|err| runtime_error(format!("{}: {err}", path.display())))?;
        Ok((parsed, raw))
    }
}

impl DaemonService {
    pub(super) fn load_config(&self, vcs_root: &Path) -> Result<SlipforkConfig, EdgeError> {
        // `slipfork.toml` is optional (`VISION.md`, `docs/getting-started.md`): an absent manifest
        // means the default DAG/resources spec, while a malformed or unreadable one fails closed.
        load_optional_slipfork_config(&vcs_root.join("slipfork.toml")).map_err(runtime_error)
    }
}

impl DaemonService {
    /// Materialize (or reuse) the selected `snow.yml` `Dockerfile` stage as the world's `L0`
    /// rootfs (`ADR-0009` §1–§3), resolving `[world]` and every requested backing-service image.
    pub(super) fn materialize_rootfs(
        &self,
        inputs: RootfsInputs<'_>,
    ) -> Result<WorldRootfs, EdgeError> {
        let RootfsInputs {
            worktree,
            snow,
            config,
            vault,
            coords,
            claims,
            host_caches,
        } = inputs;
        let build_images: Vec<&slipfork_snow::BuildImage> = snow
            .images
            .iter()
            .filter_map(|image| match image {
                Image::Build(build) => Some(build),
                Image::DockerHub { .. } => None,
            })
            .collect();
        let ids: Vec<String> = build_images.iter().map(|image| image.id.clone()).collect();
        let image_id = config
            .world
            .select_image(&ids)
            .map_err(|err| runtime_error(err.to_string()))?
            .to_owned();
        let image = build_images
            .iter()
            .find(|image| image.id == image_id)
            .copied()
            .ok_or_else(|| {
                runtime_error(format!(
                    "snow.yml images: selected image '{image_id}' is not a buildable Dockerfile image"
                ))
            })?;
        let context_dir = worktree.join(&image.context);
        let dockerfile = context_dir.join(&image.dockerfile);

        let groups = snow
            .effective_env_groups(WORLD_MODE)
            .map_err(|err| runtime_error(err.to_string()))?;
        let build_names: BTreeSet<String> = image
            .envs
            .iter()
            .filter_map(|group| groups.get(group))
            .flat_map(|vars| vars.keys().cloned())
            .collect();
        let owns = |namespace: &str, path: &[String]| claims.owns(namespace, path);
        let resolved = ClaimedEvaluator::new(vault, coords, self.entropy.as_ref(), &owns)
            .mode(snow, WORLD_MODE, None)
            .map_err(snow_error)?;
        let mut build_envs: Vec<BuildEnvVar> = Vec::new();
        for (name, value) in resolved
            .iter()
            .filter(|(name, _)| build_names.contains(*name))
        {
            if value.expose().contains("${{") {
                return Err(runtime_error(format!(
                    "`snow.yml` build variable '{name}' has no locally resolvable value; a Dockerfile build cannot receive an OSPP-resolved template"
                )));
            }
            build_envs.push(BuildEnvVar {
                name: name.clone(),
                value: value.expose().to_owned(),
            });
        }

        let component = snow
            .components
            .iter()
            .find(|component| component.image == image_id);
        let ports = SnowPortContract {
            component: component.map_or_else(|| image_id.clone(), |component| component.id.clone()),
            internal_ports: component
                .map(|component| component.internal_ports.clone())
                .unwrap_or_default(),
            ingress_ports: snow
                .ingresses
                .iter()
                .filter(|ingress| {
                    component.is_some_and(|component| ingress.target.component == component.id)
                })
                .map(|ingress| ingress.target.port)
                .collect(),
        };

        let request = WorldRootfsRequest {
            state_dir: self.config.layout.state_dir().to_path_buf(),
            image_id,
            context_dir,
            context_label: image.context.clone(),
            dockerfile,
            target_stage: config.world.stage.clone(),
            build_envs,
            target_arch: self.config.target_arch.clone(),
            services: requested_service_images(snow),
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
                // Progress reaches the CLI through `BootEvent`; a cache hit/miss is silent for now.
                let _ = event;
            })
            .map_err(|err| runtime_error(err.to_string()))
    }
}

impl DaemonService {
    pub(super) fn world_env(&self, inputs: WorldEnvInputs<'_>) -> Result<ExecEnv, EdgeError> {
        let WorldEnvInputs {
            snow,
            vault,
            coords,
            project,
            target,
            claims,
            ospp_env,
        } = inputs;
        let mut env = ExecEnv::world_scoped(
            self.config
                .layout
                .world_home(project, &world_id(project, target)),
        );
        for (var, value) in &self.config.inherited {
            env.inherit(*var, value.clone());
        }
        let owns = |namespace: &str, path: &[String]| claims.owns(namespace, path);
        let evaluator = ClaimedEvaluator::new(vault, coords, self.entropy.as_ref(), &owns);
        let resolved = evaluator.mode(snow, WORLD_MODE, None).map_err(snow_error)?;
        apply_env(&mut env, resolved)?;
        if let Some(components) = ospp_env {
            for server in components.values() {
                for (name, value) in server {
                    let rendered = evaluator.render(value.expose()).map_err(snow_error)?;
                    let secret = value.is_secret() || rendered.is_secret();
                    let raw = rendered.expose().to_owned();
                    let outcome = if secret {
                        env.set_secret(name, raw)
                    } else {
                        env.set(name, raw)
                    };
                    outcome.map_err(|err| {
                        runtime_error(format!("invalid OSPP environment '{name}': {err}"))
                    })?;
                }
            }
        }
        Ok(env)
    }
}

impl DaemonService {
    /// Resolve every `snow.yml`- and flag-derived fact one pipeline run needs, before anything is
    /// materialized or started.
    ///
    /// `boot` and `reboot` call this same resolver, so a `restart` re-derives the resource plan,
    /// resource coordinates, origin, environments, enabled components, and ingress/domain table
    /// from the edited file instead of reusing boot-time values.
    pub(super) fn resolve_world(
        &self,
        repo: &RepoRoot,
        target: &WorldTarget,
        worktree: &Path,
        policy: BootPolicy,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<ResolvedWorld, EdgeError> {
        let project = &repo.project;
        let (snow, snow_source) = self.load_snow(worktree)?;
        let effective = snow
            .effective_components(WORLD_MODE)
            .map_err(|err| runtime_error(err.to_string()))?;
        let route = LocalHostRoute::new(project.clone(), target.clone(), None, &snow.domain_ids())
            .map_err(|err| runtime_error(err.to_string()))?;
        let origin = self.registry.mint_origin(&route, self.ingress.port())?;
        let config = self.load_config(&repo.vcs_root)?;

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
            &config,
            &snow,
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
        // Resource coordinates are always world-local: remote S3 credentials arrive as component
        // environment values, and PostgreSQL is still the local container restored from a stream.
        let coords =
            ResourceCoordinates::for_world(&snow, project, target, &origin, &BTreeMap::new())
                .map_err(|err| runtime_error(err.to_string()))?;
        let vault = self.vault_for(project)?;
        let claims = self
            .ospp
            .claims_for(project, &snow, &plan, self.vault_store(project)?)?;
        let ospp_env = match (session.as_ref(), remote.as_deref()) {
            (Some(_), Some(remote)) => {
                self.ospp
                    .subscribe(project, remote, &snow_source, &claims)?
            }
            _ => None,
        };
        let base_env = self.world_env(WorldEnvInputs {
            snow: &snow,
            vault: vault.as_ref(),
            coords: &coords,
            project,
            target,
            claims: &claims,
            ospp_env: ospp_env.as_ref().map(|env| &env.components),
        })?;
        let mut components = Vec::new();
        for (id, resolved) in &effective {
            if resolved.disabled {
                continue;
            }
            let readiness_port = snow
                .components
                .iter()
                .find(|component| component.id == *id)
                .and_then(|component| component.internal_ports.first().copied());
            components.push(ResolvedComponent {
                id: id.clone(),
                run_command: resolved.run_command.clone(),
                readiness_port,
                env: resolve_component_env(
                    &snow,
                    vault.as_ref(),
                    &coords,
                    id,
                    &claims,
                    ospp_env.as_ref().and_then(|env| env.component(id)),
                    self.entropy.as_ref(),
                )?,
            });
        }
        let ingresses = ingress_routes(&snow, &effective)?;
        let domains = snow.domain_ids().into_iter().map(str::to_owned).collect();
        Ok(ResolvedWorld {
            snow,
            config,
            vault,
            plan,
            resources,
            coords,
            base_env,
            components,
            ingresses,
            domains,
            claims,
            ospp_env: ospp_env.map(|environment| environment.components),
        })
    }
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
        worktree: &Path,
        policy: BootPolicy,
        on_event: &mut dyn FnMut(BootEvent),
    ) -> Result<PreparedWorld, EdgeError> {
        let resolved = self.resolve_world(repo, target, worktree, policy, on_event)?;
        let rootfs = self.materialize_rootfs(RootfsInputs {
            worktree,
            snow: &resolved.snow,
            config: &resolved.config,
            vault: resolved.vault.as_ref(),
            coords: &resolved.coords,
            claims: &resolved.claims,
            host_caches: policy.host_caches,
        })?;
        let (snapshot, boot) =
            self.ensure_base_snapshot(&repo.project, &resolved.snow, &resolved.plan, &rootfs)?;
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
        worktree: &Path,
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

impl DaemonService {
    pub(super) fn snow_resolve(
        &self,
        req: &SnowResolveRequest,
    ) -> Result<SnowResolveSummary, EdgeError> {
        let file = if req.file.is_absolute() {
            req.file.clone()
        } else {
            req.cwd.join(&req.file)
        };
        let raw = fs::read_to_string(&file)
            .map_err(|err| runtime_error(format!("cannot read {}: {err}", file.display())))?;
        let snow = SnowFile::parse_yaml(&raw)
            .map_err(|err| runtime_error(format!("{}: {err}", file.display())))?;
        let repository = self
            .repo_roots
            .resolve(&parent_dir(&file))
            .map_err(|err| runtime_error(err.to_string()))?;
        let project = repository.project;
        let vault = self.vault_for(&project)?;
        let coords = ResourceCoordinates::local_for_world(&snow, &project, &WorldTarget::Canonical)
            .map_err(|err| runtime_error(err.to_string()))?;
        let resolved = snow
            .resolve_mode(
                &req.mode,
                req.component.as_deref(),
                vault.as_ref(),
                &coords,
                &OsCsprngEntropy,
            )
            .map_err(|err| runtime_error(err.to_string()))?;
        let vars = resolved
            .iter()
            .map(|(key, value)| {
                let text = if req.expose_secrets {
                    value.expose()
                } else {
                    value.redacted()
                };
                (key.clone(), text.to_owned())
            })
            .collect();
        Ok(SnowResolveSummary { vars })
    }
}

pub(super) fn dump_policy(request: slipfork_wire::control::DumpRequest) -> DumpPolicy {
    match request {
        slipfork_wire::control::DumpRequest::Default => DumpPolicy::Default,
        slipfork_wire::control::DumpRequest::NoDump => DumpPolicy::NoDump,
        slipfork_wire::control::DumpRequest::RefreshDump => DumpPolicy::RefreshDump,
    }
}

pub(super) fn resolve_plan(
    config: &SlipforkConfig,
    snow: &SnowFile,
    target: &WorldTarget,
    dump: DumpPolicy,
    force_local: bool,
    ospp_paired: bool,
) -> Result<ResourcePlan, EdgeError> {
    let s3_ids: Vec<&str> = snow
        .resources
        .s3_buckets
        .iter()
        .map(|bucket| bucket.id.as_str())
        .collect();
    let pg_ids: Vec<&str> = snow
        .resources
        .pg_databases
        .iter()
        .map(|database| database.id.as_str())
        .collect();
    config
        .resources
        .validate_declared_ids(&s3_ids, &pg_ids)
        .map_err(|err| runtime_error(err.to_string()))?;

    // `--resources local` is a per-invocation override (`ADR-0008` §3): the typed forced-strategy
    // spec keeps `ResourcePlan::resolve` the sole decision point while no seed dumps are used and
    // no OSPP offer is consulted.
    let spec = config.resources.with_declared_ids(&s3_ids, &pg_ids);
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
            strategy: json_label(&diagnostic.strategy),
            resolved: json_label(&diagnostic.resolved),
            reason: json_label(&diagnostic.reason),
        })
        .collect()
}
