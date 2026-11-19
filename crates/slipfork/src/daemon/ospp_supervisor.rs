//! The daemon's one OSPP client: live session, subscriptions, pairing, and seed-dump streams.
//!
//! [`OsppSupervisor`] owns the injected [`OsppClient`], the single live `ospp-v1` session, the
//! stable per-project subscriptions, the pushed `env_updated` channel, and the public pairing
//! record. `env_updated` application stays a daemon-graph verdict (`apply_env_update` takes the
//! caller), so no world state is duplicated here.

use super::pairing_store::{PairingRecord, PairingStore};
use super::service::DaemonService;
use super::types::OsppRuntime;
use super::{rejected, runtime_error};
use crate::browser::{APPROVAL_HINT_PREFIX, Browser};
use slipfork_dag::{ResourcePlan, ResourceStrategy};
use slipfork_edge::EdgeError;
use slipfork_forkfile::EvaluatedFork;
use slipfork_ospp::{
    DeviceKeySigner, OsppClaims, OsppClient, OsppError, OsppSession, SubscribeRequest,
    SubscriptionEnv,
};
use slipfork_wire::control::{AuthSummary, ControlSecret};
use slipfork_world::ProjectSlug;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;

/// Stable per-project subscription id, reused on every re-`subscribe`.
pub(super) fn subscription_id(project: &ProjectSlug) -> String {
    format!("sub-{}", project.as_str())
}

/// Append the one remedy a protocol-version refusal needs.
fn unsupported_version_remedy(line: String, error: &OsppError) -> String {
    if error.code() == Some(slipfork_ospp::codes::UNSUPPORTED_VERSION) {
        format!(
            "{line}; next: upgrade the OSPP provider to protocol v2 (or run a matching slipforkd)"
        )
    } else {
        line
    }
}

/// The daemon-held OSPP client, its live session, and the public pairing record.
pub(super) struct OsppSupervisor {
    client: Option<Arc<dyn OsppClient>>,
    signer: Arc<dyn DeviceKeySigner>,
    hostname: String,
    browser: Browser,
    pairing: PairingStore,
    runtime: Mutex<OsppRuntime>,
    updates_tx: Arc<Mutex<mpsc::Sender<SubscriptionEnv>>>,
    updates_rx: Mutex<Option<mpsc::Receiver<SubscriptionEnv>>>,
    /// One actionable, secret-free line describing a failed live session.
    failure: Mutex<Option<String>>,
}

impl OsppSupervisor {
    /// Bind the supervisor to the composition root's already-resolved OSPP collaborators.
    pub(super) fn new(
        client: Option<Arc<dyn OsppClient>>,
        signer: Arc<dyn DeviceKeySigner>,
        hostname: String,
        browser: Browser,
        pairing: PairingStore,
    ) -> Self {
        let (updates_tx, updates_rx) = mpsc::channel();
        Self {
            client,
            signer,
            hostname,
            browser,
            pairing,
            runtime: Mutex::new(OsppRuntime::default()),
            updates_tx: Arc::new(Mutex::new(updates_tx)),
            updates_rx: Mutex::new(Some(updates_rx)),
            failure: Mutex::new(None),
        }
    }

    /// Record one actionable, secret-free line describing a failed live session.
    pub(super) fn record_failure(&self, line: String) {
        let mut failure = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
        if failure.as_deref() != Some(line.as_str()) {
            eprintln!("slipforkd: {line}");
            *failure = Some(line);
        }
    }

    fn clear_failure(&self) {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The daemon-held `ospp-v1` session, dialed once and reused while it lives.
    ///
    /// An endpoint that is unreachable or refuses the machine key yields `None` so a purely local
    /// world still boots (`OSPP` is never a prerequisite, `ADR-0005` §1). A resource strategy that
    /// needs the session then fails closed inside `ResourcePlan::resolve`, and the recorded line
    /// names the endpoint and the remedy.
    pub(super) fn session(&self) -> Result<Option<Arc<dyn OsppSession>>, EdgeError> {
        let Some(client) = self.client.as_ref() else {
            return Ok(None);
        };
        {
            let runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(session) = runtime.session.as_ref() {
                return Ok(Some(Arc::clone(session)));
            }
        }
        let updates = Arc::clone(&self.updates_tx);
        let on_update = Box::new(move |environment: SubscriptionEnv| {
            if let Ok(sender) = updates.lock() {
                let _ = sender.send(environment);
            }
        });
        let session: Arc<dyn OsppSession> = match client.connect(on_update) {
            Ok(session) => Arc::from(session),
            Err(error) => {
                self.record_failure(unsupported_version_remedy(
                    format!(
                        "the OSPP endpoint {} is unavailable ({error}); run `slipfork auth` to pair this machine, then `slipfork restart`",
                        client.endpoint()
                    ),
                    &error,
                ));
                return Ok(None);
            }
        };
        self.clear_failure();
        let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
        runtime.session = Some(Arc::clone(&session));
        Ok(Some(session))
    }

    /// The ID-only claims the local world owns: local vault `vars`/`secrets`, locally provisioned
    /// resources, and Slipfork-assigned domains. Values never cross the wire.
    pub(super) fn claims_for(
        &self,
        project: &ProjectSlug,
        fork: &EvaluatedFork,
        plan: &ResourcePlan,
        store: Arc<dyn slipfork_edge::SecretVault>,
    ) -> Result<OsppClaims, EdgeError> {
        let mut resources: BTreeMap<String, Vec<String>> = BTreeMap::new();
        // Slipfork always assigns the local world's origin, so `resources::app.*` stays local.
        resources.insert(
            "app".to_owned(),
            vec!["url".to_owned(), "domain".to_owned()],
        );
        let pg_ids: Vec<String> = fork
            .pg_databases()
            .into_iter()
            .map(|database| database.database)
            .collect();
        if !pg_ids.is_empty() {
            resources.insert("pg_databases".to_owned(), pg_ids);
        }
        let redis_ids: Vec<String> = fork
            .redis
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
        secrets.extend(fork.generated_secret_keys());
        Ok(OsppClaims {
            vars: store.vars(project)?,
            secrets: secrets.into_iter().collect(),
            resources,
            domains: fork.domains.clone(),
            // Components are always started locally, but their *environment* is exactly what the
            // server resolves; claiming one would suppress its `env_resolved` entry.
            components: Vec::new(),
        })
    }

    /// Subscribe (or re-subscribe with the same stable id) for one project.
    pub(super) fn subscribe(
        &self,
        project: &ProjectSlug,
        remote: &str,
        forkfile: &EvaluatedFork,
        claims: &OsppClaims,
    ) -> Result<Option<SubscriptionEnv>, EdgeError> {
        let Some(session) = self.session()? else {
            return Ok(None);
        };
        let id = subscription_id(project);
        let request = SubscribeRequest {
            id: id.clone(),
            remote: remote.to_owned(),
            forkfile: forkfile.clone(),
            claims: claims.clone(),
        };
        match session.subscribe(&request) {
            Ok(environment) => {
                let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
                runtime.subscriptions.insert(id, project.clone());
                Ok(Some(environment))
            }
            Err(error) => {
                self.record_failure(unsupported_version_remedy(
                    format!(
                        "the OSPP subscription for project '{project}' failed ({error}); run `slipfork restart` after fixing the server"
                    ),
                    &error,
                ));
                Err(runtime_error(unsupported_version_remedy(
                    format!("cannot subscribe project '{project}' to OSPP: {error}"),
                    &error,
                )))
            }
        }
    }

    /// The decrypted stream behind one `dump_ready` request for a declared `pg_databases` entry.
    pub(super) fn dump(
        &self,
        session: &Arc<dyn OsppSession>,
        project: &ProjectSlug,
        database: &str,
    ) -> Result<Box<dyn Read + Send>, OsppError> {
        session.dump(
            &subscription_id(project),
            &format!("pg_databases.{database}"),
        )
    }

    /// Drop the session once the project's last world stops (`unsubscribe`, then `close`).
    pub(super) fn release_project(&self, project: &ProjectSlug, still_attached: bool) {
        if still_attached {
            return;
        }
        let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
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

    /// Take the one pushed-environment receiver the supervisor thread drains.
    pub(super) fn take_updates(&self) -> Option<mpsc::Receiver<SubscriptionEnv>> {
        self.updates_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// The project one active subscription belongs to (`None` = unknown pushed id).
    pub(super) fn project_for(&self, subscription: &str) -> Option<ProjectSlug> {
        self.runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .subscriptions
            .get(subscription)
            .cloned()
    }

    /// Pair this machine and persist only the public `{ endpoint, account, machine }` record.
    pub(super) fn auth_login(&self, endpoint_url: Option<&str>) -> Result<AuthSummary, EdgeError> {
        let Some(client) = self.client.as_ref() else {
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
            let mut runtime = self.runtime.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(session) = runtime.session.take() {
                let _ = session.close();
            }
        }
        // Only the public pairing coordinates reach disk: no key, token, or claim material.
        self.pairing
            .record(&PairingRecord {
                endpoint: client.endpoint().as_str().to_owned(),
                account: approval.account.clone(),
                machine: approval.machine.clone(),
            })
            .map_err(|err| {
                runtime_error(format!(
                    "cannot record the OSPP pairing at {}: {err}; next: make the state directory writable and re-run `slipfork auth`",
                    self.pairing.path().display()
                ))
            })?;
        Ok(AuthSummary {
            key_id: format!("p256:{}", self.signer.public_key_hex()),
            backend: "software:file-0600".to_owned(),
            account: Some(approval.account),
            machine: Some(approval.machine),
            pairing_url: ControlSecret::new(
                opened.ok_or_else(|| runtime_error("the OSPP server issued no pairing handle"))?,
            ),
        })
    }
}

impl DaemonService {
    /// The daemon-held pairing verb over the one OSPP supervisor.
    pub(super) fn auth_login(&self, endpoint_url: Option<&str>) -> Result<AuthSummary, EdgeError> {
        self.ospp.auth_login(endpoint_url)
    }

    /// Drop the project's session once its last attached world stops.
    pub(super) fn note_world_released(&self, project: &ProjectSlug) {
        let still_attached = {
            let state = self.lock();
            state.worlds.iter().any(|world| &world.project == project)
        };
        self.ospp.release_project(project, still_attached);
    }

    /// Start the one subscription supervisor that applies pushed `env_updated` environments.
    pub(super) fn start_ospp_supervisor(self: &Arc<Self>) {
        let Some(receiver) = self.ospp.take_updates() else {
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
                        service.ospp.record_failure(error.to_string());
                    }
                }
            });
    }
}
