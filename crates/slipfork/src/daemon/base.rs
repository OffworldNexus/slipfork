//! Base snapshot identity: backing-service versions plus every resolved dump digest.

use sha2::{Digest, Sha256};
use slipfork_dag::{PgSeed, ResourcePlan};
use slipfork_forkfile::EvaluatedFork;
use slipfork_world::BaseLineage;
use slipfork_world::SeedLineage;
use slipfork_world::{BootLineage, ProjectSlug};

use super::backing::requested_service_images;

/// Content key of one base snapshot: backing-service versions plus every resolved dump digest.
/// The seed dumps the plan selected: `(any_seed, single_digest)`.
///
/// `false` means the plan restores no dump (a clean `initdb` base); several dumps are a seed with
/// no single digest, so the digest stays `None` rather than naming one database arbitrarily.
pub(super) fn selected_seed(fork: &EvaluatedFork, plan: &ResourcePlan) -> (bool, Option<String>) {
    let declared: Vec<String> = fork
        .pg_databases()
        .into_iter()
        .map(|database| database.database)
        .collect();
    let has_seed = plan.pg_databases.iter().any(|database| {
        declared.contains(&database.id) && matches!(database.seed, PgSeed::RemoteDump { .. })
    });
    // OSPP v1 no longer advertises a dump digest, so the only honest lineage label is `remote`.
    (has_seed, has_seed.then(|| "remote".to_owned()))
}

/// The facts a world reuses when the base snapshot already existed on disk.
///
/// No seal ran for this world (`base.millis: None`) and no seed was rebuilt
/// (`seed.rebuilt: Some(false)`), so a restarted daemon never claims another boot's duration.
pub(super) fn reused_base_lineage(
    key: &str,
    has_seed: bool,
    digest_hex: Option<String>,
) -> BootLineage {
    BootLineage {
        base: Some(BaseLineage {
            key: key.to_owned(),
            millis: None,
        }),
        seed: has_seed.then_some(SeedLineage {
            digest: digest_hex,
            rebuilt: Some(false),
            millis: None,
        }),
    }
}

pub(super) fn base_digest(
    project: &ProjectSlug,
    fork: &EvaluatedFork,
    plan: &ResourcePlan,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"slipfork-base:v1\0");
    hasher.update(project.as_str().as_bytes());
    hasher.update(b"\0");
    for image in requested_service_images(fork) {
        hasher.update(image.to_string().as_bytes());
        hasher.update(b"\0");
    }
    for database in &plan.pg_databases {
        hasher.update(database.id.as_bytes());
        hasher.update(b"=");
        match &database.seed {
            PgSeed::CleanInitdb => hasher.update(b"clean"),
            PgSeed::RemoteDump { .. } => hasher.update(b"remote"),
        }
        hasher.update(b"\0");
    }
    hex::encode(hasher.finalize())
}
