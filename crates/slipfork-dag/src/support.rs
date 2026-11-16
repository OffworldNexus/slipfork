//! Typed integration support registry (`support/integrations.toml`, `ADR-0007`).
//!
//! The registry file is the sole source of truth for "how well does Slipfork support X": every
//! entry declares a category, an ordered [`IntegrationTier`], and typed [`Evidence`] pointers.
//! [`SupportRegistry::parse_toml`] is the only constructor and validates the in-record contract
//! (a claimed tier must carry its required non-empty pointers, ids are unique, the registry is
//! non-empty); verifying those pointers on disk, in `git`, and in CI workflows is the M2
//! compliance tool's job (`ADR-0007` §2–§3).

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize, Serializer};
use thiserror::Error;

/// Errors produced when parsing or validating `support/integrations.toml`.
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("invalid support/integrations.toml: {0}")]
    Toml(String),
    #[error("support/integrations.toml must declare at least one [[integration]] entry")]
    EmptyRegistry,
    #[error("duplicate integration id '{0}'")]
    DuplicateId(String),
    #[error(
        "integration '{id}' claims tier '{tier}' but evidence field '{field}' is missing or empty"
    )]
    MissingEvidence {
        id: String,
        tier: IntegrationTier,
        field: EvidenceField,
    },
}

/// Ordered support scale: `none < declared < exercised < field-tested < hardened` (`ADR-0007` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationTier {
    /// No meaningful test surface; attested with a rationale and a signed-off decision record.
    None,
    /// Manifest recipe written and pinned by a critical test, never exercised outside its author.
    Declared,
    /// CI runs a real project through the integration.
    Exercised,
    /// A real team/product runs its suite through the integration.
    FieldTested,
    /// Field-tested at scale, including browser/multi-worker/flaky-suite behaviour.
    Hardened,
}

impl IntegrationTier {
    /// Evidence pointers this tier requires, cumulative from `none` to `hardened` (`ADR-0007` §1).
    ///
    /// This is the single tier→evidence ladder: validation, the critical test, and the M2
    /// compliance tool all read it instead of restating the table.
    pub fn required_evidence(self) -> &'static [EvidenceField] {
        match self {
            Self::None => &[
                EvidenceField::Rationale,
                EvidenceField::SignoffDoc,
                EvidenceField::SignoffCommit,
            ],
            Self::Declared => &[EvidenceField::RecipePage, EvidenceField::CriticalTest],
            Self::Exercised => &[
                EvidenceField::RecipePage,
                EvidenceField::CriticalTest,
                EvidenceField::CiJob,
            ],
            Self::FieldTested => &[
                EvidenceField::RecipePage,
                EvidenceField::CriticalTest,
                EvidenceField::CiJob,
                EvidenceField::DeployRef,
            ],
            Self::Hardened => &[
                EvidenceField::RecipePage,
                EvidenceField::CriticalTest,
                EvidenceField::CiJob,
                EvidenceField::DeployRef,
                EvidenceField::ProductionSuiteRef,
                EvidenceField::TestAudit,
            ],
        }
    }
}

impl fmt::Display for IntegrationTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Declared => "declared",
            Self::Exercised => "exercised",
            Self::FieldTested => "field-tested",
            Self::Hardened => "hardened",
        })
    }
}

/// Integration class an entry belongs to (`ADR-0007` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationCategory {
    /// Hypervisor / CoW world engine (e.g. `ReflinkNetnsEngine`, `IncusEngine`).
    WorldEngine,
    /// Secret provider / vault integration (e.g. `FileVaultProvider`, `OSPP v1`).
    Secrets,
    /// Outbound tunnel registrar (e.g. `weaved`).
    Tunnel,
    /// VCS workspace driver (e.g. Git worktrees, Jujutsu workspaces).
    Vcs,
    /// Language/framework test runner adapter (e.g. `pytest-slipfork`, Playwright, PHPUnit).
    TestRunner,
    /// Slipfork CLI surface (e.g. `slipfork dag plan`).
    Cli,
}

/// One evidence pointer kind; the single tier→evidence vocabulary (`ADR-0007` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceField {
    /// Why an integration has no meaningful test surface (`none`).
    Rationale,
    /// Existing ADR/review document recording the sign-off (`none`).
    SignoffDoc,
    /// 40-hex commit SHA recording the sign-off (`none`).
    SignoffCommit,
    /// Existing published documentation page showing the manifest recipe (`declared`).
    RecipePage,
    /// `<crate>::<critical_file>::<test_fn>` symbol in `crates/*/tests/critical_*.rs` (`declared`).
    CriticalTest,
    /// `<workflow.yml>#<job_id>` present in `.github/workflows/` (`exercised`).
    CiJob,
    /// Committed deployment record `support/deployments/<id>.md` (`field-tested`).
    DeployRef,
    /// Committed scale record (`hardened`).
    ProductionSuiteRef,
    /// `docs/testing/AUDIT-*.md` audit document (`hardened`).
    TestAudit,
}

impl EvidenceField {
    /// Every evidence pointer kind, in the order the `[integration.evidence]` table documents.
    ///
    /// Consumers iterate this to verify **all present** pointers, not only the tier-required ones.
    pub const ALL: &[EvidenceField] = &[
        Self::Rationale,
        Self::SignoffDoc,
        Self::SignoffCommit,
        Self::RecipePage,
        Self::CriticalTest,
        Self::CiJob,
        Self::DeployRef,
        Self::ProductionSuiteRef,
        Self::TestAudit,
    ];

    /// Stable wire/display name; `Display`, serde, and `slipfork support --json` all use it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rationale => "rationale",
            Self::SignoffDoc => "signoff_doc",
            Self::SignoffCommit => "signoff_commit",
            Self::RecipePage => "recipe_page",
            Self::CriticalTest => "critical_test",
            Self::CiJob => "ci_job",
            Self::DeployRef => "deploy_ref",
            Self::ProductionSuiteRef => "production_suite_ref",
            Self::TestAudit => "test_audit",
        }
    }
}

impl Serialize for EvidenceField {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl fmt::Display for EvidenceField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One flat record of optional evidence pointers; `tier` decides which pointers must be present.
///
/// A single flat record (rather than a per-tier sum type) lets `make support-sync` demote an entry
/// in place without reshaping its TOML record (`ADR-0007` §3). Fields are private and the type has
/// no `Deserialize`: [`SupportRegistry::parse_toml`] is the sole constructor, so an unvalidated
/// claim cannot exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    rationale: Option<String>,
    signoff_doc: Option<String>,
    signoff_commit: Option<String>,
    recipe_page: Option<String>,
    critical_test: Option<String>,
    ci_job: Option<String>,
    deploy_ref: Option<String>,
    production_suite_ref: Option<String>,
    test_audit: Option<String>,
}

impl Evidence {
    /// The raw pointer for `field`, if present.
    pub fn field(&self, field: EvidenceField) -> Option<&str> {
        match field {
            EvidenceField::Rationale => self.rationale.as_deref(),
            EvidenceField::SignoffDoc => self.signoff_doc.as_deref(),
            EvidenceField::SignoffCommit => self.signoff_commit.as_deref(),
            EvidenceField::RecipePage => self.recipe_page.as_deref(),
            EvidenceField::CriticalTest => self.critical_test.as_deref(),
            EvidenceField::CiJob => self.ci_job.as_deref(),
            EvidenceField::DeployRef => self.deploy_ref.as_deref(),
            EvidenceField::ProductionSuiteRef => self.production_suite_ref.as_deref(),
            EvidenceField::TestAudit => self.test_audit.as_deref(),
        }
    }

    /// `true` when `field` carries a non-whitespace pointer (the single presence predicate).
    pub fn is_present(&self, field: EvidenceField) -> bool {
        self.field(field)
            .is_some_and(|value| !value.trim().is_empty())
    }
}

/// One integration support claim; constructed only by [`SupportRegistry::parse_toml`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationEntry {
    id: String,
    category: IntegrationCategory,
    tier: IntegrationTier,
    evidence: Evidence,
}

impl IntegrationEntry {
    /// Stable integration id (`kebab-case`).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Integration class.
    pub fn category(&self) -> IntegrationCategory {
        self.category
    }

    /// Claimed support tier.
    pub fn tier(&self) -> IntegrationTier {
        self.tier
    }

    /// Typed evidence pointers backing the claimed tier.
    pub fn evidence(&self) -> &Evidence {
        &self.evidence
    }
}

/// Parsed `support/integrations.toml` registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportRegistry {
    entries: Vec<IntegrationEntry>,
}

/// Raw `[[integration]]` record; `Deserialize` lives only here, never on the public claim types.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIntegrationEntry {
    id: String,
    category: IntegrationCategory,
    tier: IntegrationTier,
    evidence: RawEvidence,
}

impl From<RawIntegrationEntry> for IntegrationEntry {
    fn from(raw: RawIntegrationEntry) -> Self {
        Self {
            id: raw.id,
            category: raw.category,
            tier: raw.tier,
            evidence: raw.evidence.into(),
        }
    }
}

/// Raw `[integration.evidence]` table; `deny_unknown_fields` keeps typos hard errors.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvidence {
    rationale: Option<String>,
    signoff_doc: Option<String>,
    signoff_commit: Option<String>,
    recipe_page: Option<String>,
    critical_test: Option<String>,
    ci_job: Option<String>,
    deploy_ref: Option<String>,
    production_suite_ref: Option<String>,
    test_audit: Option<String>,
}

impl From<RawEvidence> for Evidence {
    fn from(raw: RawEvidence) -> Self {
        Self {
            rationale: raw.rationale,
            signoff_doc: raw.signoff_doc,
            signoff_commit: raw.signoff_commit,
            recipe_page: raw.recipe_page,
            critical_test: raw.critical_test,
            ci_job: raw.ci_job,
            deploy_ref: raw.deploy_ref,
            production_suite_ref: raw.production_suite_ref,
            test_audit: raw.test_audit,
        }
    }
}

/// Raw wire document; `[[integration]]` array-of-tables.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegistry {
    #[serde(default)]
    integration: Vec<RawIntegrationEntry>,
}

impl SupportRegistry {
    /// Load the registry embedded at compile time (`include_str!`), with zero ambient cwd I/O.
    pub fn embedded() -> Result<Self, RegistryError> {
        Self::parse_toml(include_str!("../../../support/integrations.toml"))
    }

    /// Sole parser and validating path for the CLI, the M2 compliance tool, and generated docs.
    pub fn parse_toml(raw: &str) -> Result<Self, RegistryError> {
        let raw: RawRegistry =
            toml::from_str(raw).map_err(|error| RegistryError::Toml(error.to_string()))?;
        let registry = Self {
            entries: raw.integration.into_iter().map(Into::into).collect(),
        };
        registry.validate()?;
        Ok(registry)
    }

    /// Every registry entry in file order.
    pub fn entries(&self) -> &[IntegrationEntry] {
        &self.entries
    }

    /// Reject an empty registry, duplicate ids, and any claimed tier missing a required pointer.
    fn validate(&self) -> Result<(), RegistryError> {
        if self.entries.is_empty() {
            return Err(RegistryError::EmptyRegistry);
        }
        let mut seen = BTreeSet::new();
        for entry in &self.entries {
            if !seen.insert(entry.id.as_str()) {
                return Err(RegistryError::DuplicateId(entry.id.clone()));
            }
            for &field in entry.tier.required_evidence() {
                if !entry.evidence.is_present(field) {
                    return Err(RegistryError::MissingEvidence {
                        id: entry.id.clone(),
                        tier: entry.tier,
                        field,
                    });
                }
            }
        }
        Ok(())
    }
}
