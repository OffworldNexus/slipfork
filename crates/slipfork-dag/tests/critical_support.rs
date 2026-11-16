//! Protected critical tests for the embedded integration support registry (`ADR-0007`).
//!
//! `0.1.0` verifies that every claim parses through the typed schema, that ids are unique, that
//! every `declared`+ `recipe_page`/`signoff_doc` path exists on disk, and that every
//! `critical_test` symbol resolves. `signoff_commit` is only checked for 40-hex shape; `git`
//! commit existence and CI-workflow job keys are the M2 compliance tool's job.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use slipfork_dag::{EvidenceField, IntegrationTier, RegistryError, SupportRegistry};

/// Repository root derived from the compile-time crate location (never the ambient cwd).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/slipfork-dag lives two levels below the repository root")
        .to_path_buf()
}

fn assert_pointer_exists(pointer: &str) {
    let path = repo_root().join(pointer);
    assert!(
        path.is_file(),
        "evidence pointer '{pointer}' must exist at {}",
        path.display()
    );
}

/// Assert `<crate>::<critical_file>::<test_fn>` names a real function in `crates/*/tests/`.
fn assert_critical_test_symbol(symbol: &str) {
    let mut parts = symbol.split("::");
    let crate_name = parts.next().expect("crate segment");
    let file_stem = parts.next().expect("critical test file segment");
    let test_fn = parts.next().expect("test function segment");
    assert!(
        parts.next().is_none(),
        "critical_test '{symbol}' must be exactly <crate>::<critical_file>::<test_fn>"
    );
    assert!(
        file_stem.starts_with("critical_"),
        "critical_test '{symbol}' must live in a protected critical_* test file"
    );
    let crate_dir = crate_name.replace('_', "-");
    let path = repo_root()
        .join("crates")
        .join(&crate_dir)
        .join("tests")
        .join(format!("{file_stem}.rs"));
    let source = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "critical_test '{symbol}' must resolve to {}: {error}",
            path.display()
        )
    });
    assert!(
        source.contains(&format!("fn {test_fn}(")),
        "critical_test '{symbol}' must name a test function in {}",
        path.display()
    );
}

const SIGNOFF_DOC: &str = "docs/decisions/ADR-0007-integration-support-tiers.md";

/// One complete `none` record; tests mutate only the id/rationale they are exercising.
fn none_record(id: &str, rationale: &str) -> String {
    format!(
        "[[integration]]\nid = \"{id}\"\ncategory = \"cli\"\ntier = \"none\"\n\n\
         [integration.evidence]\nrationale = \"{rationale}\"\n\
         signoff_doc = \"{SIGNOFF_DOC}\"\nsignoff_commit = \"{}\"\n",
        "a".repeat(40)
    )
}

fn assert_signoff_commit(commit: &str, id: &str) {
    assert_eq!(
        commit.len(),
        40,
        "integration '{id}' signoff_commit must be a 40-hex SHA"
    );
    assert!(
        commit
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "integration '{id}' signoff_commit '{commit}' must be lowercase 40-hex"
    );
}

#[test]
fn embedded_registry_parses_with_unique_ids_and_ordered_tiers() {
    let registry = SupportRegistry::embedded().expect("embedded support/integrations.toml parses");
    assert!(
        !registry.entries().is_empty(),
        "the support registry must list the shipped 0.1.0 surface"
    );

    assert!(IntegrationTier::None < IntegrationTier::Declared);
    assert!(IntegrationTier::Declared < IntegrationTier::Exercised);
    assert!(IntegrationTier::Exercised < IntegrationTier::FieldTested);
    assert!(IntegrationTier::FieldTested < IntegrationTier::Hardened);

    let mut ids = BTreeSet::new();
    for entry in registry.entries() {
        assert!(
            ids.insert(entry.id()),
            "integration id '{}' must be unique",
            entry.id()
        );
        // The shared tier→evidence ladder is the only place the requirement table lives.
        for &field in entry.tier().required_evidence() {
            assert!(
                entry.evidence().is_present(field),
                "integration '{}' (tier '{}') must carry a non-empty '{field}'",
                entry.id(),
                entry.tier()
            );
        }
    }
}

#[test]
fn embedded_registry_resolves_every_present_evidence_pointer() {
    let registry = SupportRegistry::embedded().expect("embedded support/integrations.toml parses");
    for entry in registry.entries() {
        for &field in EvidenceField::ALL {
            if !entry.evidence().is_present(field) {
                continue;
            }
            let pointer = entry.evidence().field(field).expect("present pointer");
            match field {
                EvidenceField::Rationale => assert!(
                    !pointer.trim().is_empty(),
                    "integration '{}' must state a non-empty rationale",
                    entry.id()
                ),
                EvidenceField::SignoffDoc
                | EvidenceField::RecipePage
                | EvidenceField::DeployRef
                | EvidenceField::ProductionSuiteRef
                | EvidenceField::TestAudit => assert_pointer_exists(pointer),
                EvidenceField::CriticalTest => assert_critical_test_symbol(pointer),
                EvidenceField::CiJob => {
                    let (workflow, job) = pointer
                        .split_once('#')
                        .expect("ci_job must be <workflow.yml>#<job_id>");
                    let path = repo_root().join(".github/workflows").join(workflow);
                    let source = fs::read_to_string(&path).unwrap_or_else(|error| {
                        panic!(
                            "ci_job '{pointer}' must resolve to {}: {error}",
                            path.display()
                        )
                    });
                    assert!(
                        source.contains(&format!("{job}:")),
                        "ci_job '{pointer}' must name a job key in {}",
                        path.display()
                    );
                }
                // Format-only here; `git` commit existence is the M2 compliance tool's job.
                EvidenceField::SignoffCommit => assert_signoff_commit(pointer, entry.id()),
            }
        }
    }
}

#[test]
fn evidence_field_strings_are_the_single_display_and_json_source() {
    let mut seen = BTreeSet::new();
    for &field in EvidenceField::ALL {
        assert_eq!(field.to_string(), field.as_str());
        assert!(seen.insert(field.as_str()), "evidence names must be unique");
    }
}

#[test]
fn validate_rejects_overclaiming_entries_empty_pointers_and_duplicate_ids() {
    // An empty registry fails closed instead of rendering an empty support matrix.
    for raw in ["", "integration = []\n"] {
        let error = SupportRegistry::parse_toml(raw).expect_err("empty registry fails closed");
        assert!(
            matches!(error, RegistryError::EmptyRegistry),
            "{raw:?}: {error:?}"
        );
    }

    // A `declared` claim without a recipe page / critical test is rejected, not silently accepted.
    let overclaim = r#"
[[integration]]
id = "ghost-recipe"
category = "cli"
tier = "declared"

[integration.evidence]
rationale = "no recipe page yet"
"#;
    let error = SupportRegistry::parse_toml(overclaim).expect_err("declared over-claim fails");
    assert!(
        matches!(
            error,
            RegistryError::MissingEvidence {
                field: EvidenceField::RecipePage,
                ..
            }
        ),
        "expected MissingEvidence(recipe_page), got {error:?}"
    );

    // `exercised` must carry the `declared` pointers plus a CI job.
    let missing_ci = r#"
[[integration]]
id = "ci-missing"
category = "world-engine"
tier = "exercised"

[integration.evidence]
recipe_page = "docs/getting-started.md"
critical_test = "slipfork_engine::critical_engine::r10_tmpfs_fails_closed_without_silent_byte_copy_fallback"
"#;
    let error = SupportRegistry::parse_toml(missing_ci).expect_err("exercised over-claim fails");
    assert!(
        matches!(
            error,
            RegistryError::MissingEvidence {
                field: EvidenceField::CiJob,
                ..
            }
        ),
        "expected MissingEvidence(ci_job), got {error:?}"
    );

    // Whitespace-only evidence is as absent as a missing pointer.
    let blank_rationale = none_record("blank-rationale", "   ");
    let error =
        SupportRegistry::parse_toml(&blank_rationale).expect_err("blank rationale fails closed");
    assert!(
        matches!(
            error,
            RegistryError::MissingEvidence {
                field: EvidenceField::Rationale,
                ..
            }
        ),
        "expected MissingEvidence(rationale), got {error:?}"
    );

    // Duplicate ids are rejected even when both records are otherwise complete.
    let duplicate = format!(
        "{}{}",
        none_record("same-id", "first"),
        none_record("same-id", "second")
    );
    let error = SupportRegistry::parse_toml(&duplicate).expect_err("duplicate ids fail closed");
    assert!(
        matches!(error, RegistryError::DuplicateId(ref id) if id == "same-id"),
        "expected DuplicateId(same-id), got {error:?}"
    );

    // Unknown evidence fields and unknown tiers stay hard TOML errors (deny_unknown_fields).
    let unknown_field = none_record("unknown-field", "x") + "ghost = \"nope\"\n";
    assert!(matches!(
        SupportRegistry::parse_toml(&unknown_field),
        Err(RegistryError::Toml(_))
    ));

    let unknown_tier = r#"
[[integration]]
id = "unknown-tier"
category = "cli"
tier = "battle-tested"
"#;
    assert!(matches!(
        SupportRegistry::parse_toml(unknown_tier),
        Err(RegistryError::Toml(_))
    ));

    // A complete `none` record is accepted.
    let valid_none = none_record("valid-none", "nothing to integrate");
    let registry = SupportRegistry::parse_toml(&valid_none).expect("valid none entry parses");
    assert_eq!(registry.entries().len(), 1);
}
