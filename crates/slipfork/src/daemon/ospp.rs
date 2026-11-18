use super::env::resolve_component_env;
use super::service::DaemonService;
use super::{WORLD_MODE, rejected, runtime_error, snow_error};
use crate::browser::APPROVAL_HINT_PREFIX;
use slipfork_dag::{ResourcePlan, ResourceStrategy};
use slipfork_edge::EdgeError;
use slipfork_ospp::SubscribeRequest;
use slipfork_ospp::{OsppClaims, OsppSession, SubscriptionEnv};
use slipfork_snow::SnowFile;
use slipfork_world::ProjectSlug;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError};
use std::thread;

/// Stable per-project subscription id, reused on every re-`subscribe`.
pub(super) fn subscription_id(project: &ProjectSlug) -> String {
    format!("sub-{}", project.as_str())
}

impl DaemonService {
    /// Record one actionable, secret-free line describing a failed live session.
    pub(super) fn record_ospp_failure(&self, line: String) {
        let mut failure = self
            .ospp_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if failure.as_deref() != Some(line.as_str()) {
            eprintln!("slipforkd: {line}");
            *failure = Some(line);
        }
    }
}

impl DaemonService {
    /// The daemon-held `ospp-v1` session, dialed once and reused while it lives.
    ///
    /// An endpoint that is unreachable or refuses the machine key yields `None` so a purely local
    /// world still boots (`OSPP` is never a prerequisite, `ADR-0005` §1). A resource strategy that
    /// needs the session then fails closed inside `ResourcePlan::resolve`, and the recorded line
    /// names the endpoint and the remedy.
    pub(super) fn ospp_session(&self) -> Result<Option<Arc<dyn OsppSession>>, EdgeError> {
        let Some(client) = self.ospp.as_ref() else {
            return Ok(None);
        };
        {
            let runtime = self
                .ospp_runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(session) = runtime.session.as_ref() {
                return Ok(Some(Arc::clone(session)));
            }
        }
        let updates = Arc::clone(&self.ospp_updates_tx);
        let on_update = Box::new(move |environment: SubscriptionEnv| {
            if let Ok(sender) = updates.lock() {
                let _ = sender.send(environment);
            }
        });
        let session: Arc<dyn OsppSession> = match client.connect(on_update) {
            Ok(session) => Arc::from(session),
            Err(error) => {
                self.record_ospp_failure(format!(
                    "the OSPP endpoint {} is unavailable ({error}); run `slipfork auth` to pair this machine, then `slipfork restart`",
                    client.endpoint()
                ));
                return Ok(None);
            }
        };
        self.record_ospp_failure_clear();
        let mut runtime = self
            .ospp_runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        runtime.session = Some(Arc::clone(&session));
        Ok(Some(session))
    }
}

impl DaemonService {
    pub(super) fn record_ospp_failure_clear(&self) {
        *self
            .ospp_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }
}

impl DaemonService {
    /// The ID-only claims the local world owns: local vault `vars`/`secrets`, locally provisioned
    /// resources, and Slipfork-assigned domains. Values never cross the wire.
    pub(super) fn claims_for(
        &self,
        project: &ProjectSlug,
        snow: &SnowFile,
        plan: &ResourcePlan,
    ) -> Result<OsppClaims, EdgeError> {
        let store = self.vault_store(project)?;
        let mut resources: BTreeMap<String, Vec<String>> = BTreeMap::new();
        // Slipfork always assigns the local world's origin, so `resources::app.*` stays local.
        resources.insert(
            "app".to_owned(),
            vec!["url".to_owned(), "domain".to_owned()],
        );
        let pg_ids: Vec<String> = snow
            .resources
            .pg_databases
            .iter()
            .map(|database| database.id.clone())
            .collect();
        if !pg_ids.is_empty() {
            resources.insert("pg_databases".to_owned(), pg_ids);
        }
        let redis_ids: Vec<String> = snow
            .resources
            .redis_clusters
            .iter()
            .map(|cluster| cluster.id.clone())
            .collect();
        if !redis_ids.is_empty() {
            resources.insert("redis_clusters".to_owned(), redis_ids);
        }
        let local_s3: Vec<String> = plan
            .s3_buckets
            .iter()
            .filter(|bucket| matches!(bucket.strategy, ResourceStrategy::Local))
            .map(|bucket| bucket.id.clone())
            .collect();
        if !local_s3.is_empty() {
            resources.insert("s3_buckets".to_owned(), local_s3);
        }
        // A local-mode `!!` secret is generated and persisted by this daemon, so the server must
        // leave it untouched even before the vault file records it.
        let mut secrets: BTreeSet<String> = store.keys(project)?.into_iter().collect();
        secrets.extend(snow.generated_secret_keys(WORLD_MODE).map_err(snow_error)?);
        Ok(OsppClaims {
            vars: store.vars(project)?,
            secrets: secrets.into_iter().collect(),
            resources,
            domains: snow.domain_ids().into_iter().map(str::to_owned).collect(),
            // Components are always started locally, but their *environment* is exactly what the
            // server resolves; claiming one would suppress its `env_resolved` entry.
            components: Vec::new(),
        })
    }
}

impl DaemonService {
    /// Subscribe (or re-subscribe with the same stable id) for one project.
    pub(super) fn subscribe_project(
        &self,
        project: &ProjectSlug,
        remote: &str,
        snow_source: &str,
        claims: &OsppClaims,
    ) -> Result<Option<SubscriptionEnv>, EdgeError> {
        let Some(session) = self.ospp_session()? else {
            return Ok(None);
        };
        let id = subscription_id(project);
        let request = SubscribeRequest {
            id: id.clone(),
            remote: remote.to_owned(),
            snow: snow_source.to_owned(),
            claims: claims.clone(),
        };
        match session.subscribe(&request) {
            Ok(environment) => {
                let mut runtime = self
                    .ospp_runtime
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                runtime.subscriptions.insert(id, project.clone());
                Ok(Some(environment))
            }
            Err(error) => {
                self.record_ospp_failure(format!(
                    "the OSPP subscription for project '{project}' failed ({error}); run `slipfork restart` after fixing the server"
                ));
                Err(runtime_error(format!(
                    "cannot subscribe project '{project}' to OSPP: {error}"
                )))
            }
        }
    }
}

impl DaemonService {
    /// Drop the session when the project's last world stops (`unsubscribe`, then `close`).
    pub(super) fn note_world_released(&self, project: &ProjectSlug) {
        let remaining = {
            let state = self.lock();
            state.worlds.iter().any(|world| &world.project == project)
        };
        if remaining {
            return;
        }
        let mut runtime = self
            .ospp_runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let id = subscription_id(project);
        let released = runtime.subscriptions.remove(&id).is_some();
        if released && let Some(session) = runtime.session.as_ref() {
            let _ = session.unsubscribe(&id);
        }
        if runtime.subscriptions.is_empty()
            && let Some(session) = runtime.session.take()
        {
            let _ = session.close();
        }
    }
}

impl DaemonService {
    /// Start the one subscription supervisor that applies pushed `env_updated` environments.
    pub(super) fn start_ospp_supervisor(self: &Arc<Self>) {
        let Some(receiver) = self
            .ospp_updates_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let service = Arc::downgrade(self);
        let _ = thread::Builder::new()
            .name("ospp-supervisor".to_owned())
            .spawn(move || {
                while let Ok(environment) = receiver.recv() {
                    let Some(service) = service.upgrade() else {
                        break;
                    };
                    if let Err(error) = service.apply_env_update(&environment) {
                        service.record_ospp_failure(error.to_string());
                    }
                }
            });
    }
}

impl DaemonService {
    /// Apply one already-decrypted `env_updated` environment: recompute each component's
    /// environment from the local vault + coordinates, and restart **only** the components whose
    /// environment actually changed.
    pub(super) fn apply_env_update(&self, environment: &SubscriptionEnv) -> Result<(), EdgeError> {
        let project = {
            let runtime = self
                .ospp_runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            runtime.subscriptions.get(&environment.id).cloned()
        };
        let Some(project) = project else {
            return Err(runtime_error(format!(
                "OSPP pushed an environment for unknown subscription '{}'",
                environment.id
            )));
        };
        let _guard = self.boot_guard();
        let mut state = self.lock();
        for world in state
            .worlds
            .iter_mut()
            .filter(|world| world.project == project)
        {
            let provider = self.vault_for(&project)?;
            let claims = self.claims_for(&project, &world.snow, &world.plan)?;
            for index in 0..world.components.len() {
                let id = world.components[index].id.clone();
                let server = environment.component(&id);
                let env = resolve_component_env(
                    &world.snow,
                    provider.as_ref(),
                    &world.coords,
                    &id,
                    &claims,
                    server,
                    self.entropy.as_ref(),
                )?;
                if env == world.components[index].env {
                    continue;
                }
                let handle = world
                    .services
                    .iter()
                    .find(|service| service.id() == id)
                    .cloned();
                if let Some(handle) = handle {
                    self.engine.update_service_env(&handle, &env).map_err(|err| {
                        runtime_error(format!(
                            "cannot restart component '{id}' with the pushed OSPP environment: {err}"
                        ))
                    })?;
                }
                world.components[index].env = env;
            }
            world.ospp_env = Some(environment.components.clone());
        }
        Ok(())
    }
}

impl DaemonService {
    pub(super) fn auth_login(
        &self,
        endpoint_url: Option<&str>,
    ) -> Result<slipfork_wire::control::AuthSummary, EdgeError> {
        let Some(client) = self.ospp.as_ref() else {
            return Err(rejected(
                "this daemon has no OSPP endpoint; start `slipforkd` with `--ospp-endpoint <wss://...>`",
            ));
        };
        if let Some(requested) = endpoint_url
            && requested != client.endpoint().as_str()
        {
            return Err(rejected(format!(
                "the requested OSPP endpoint '{requested}' differs from this daemon's injected endpoint '{}'; one daemon has exactly one server identity",
                client.endpoint()
            )));
        }
        let mut opened: Option<String> = None;
        let mut on_handle = |url: &slipfork_ospp::LivePairingUrl| {
            // The handle is live only for this pairing attempt: hand it to the launcher now,
            // before `pair_approved` blocks the handshake, and show it to the developer.
            let page = url.as_str();
            eprintln!("{page}");
            match self.browser.open(page) {
                Ok(true) => {}
                Ok(false) => eprintln!("{APPROVAL_HINT_PREFIX}{page}"),
                Err(err) => return Err(slipfork_ospp::OsppError::Transport(err)),
            }
            opened = Some(page.to_owned());
            Ok(())
        };
        let approval = client
            .pair(&self.hostname, &mut on_handle)
            .map_err(|err| runtime_error(format!("OSPP pairing failed: {err}")))?;
        // The freshly paired key must be used on the next dial, never a stale session.
        {
            let mut runtime = self
                .ospp_runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(session) = runtime.session.take() {
                let _ = session.close();
            }
        }
        Ok(slipfork_wire::control::AuthSummary {
            key_id: format!("p256:{}", self.signer.public_key_hex()),
            backend: "software:file-0600".to_owned(),
            account: Some(approval.account),
            machine: Some(approval.machine),
            pairing_url: slipfork_wire::control::ControlSecret::new(
                opened.ok_or_else(|| runtime_error("the OSPP server issued no pairing handle"))?,
            ),
        })
    }
}
