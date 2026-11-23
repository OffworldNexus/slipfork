---
id: M3.69.1
title: "Forkfile built-in dev + FeatureSet resolution, threaded through world plan & ingress"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-lsp/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-ingress/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-23T13:09"
updated: "2026-11-23T13:26"
---

# Goal

Make `dev` a **built-in** `Forkfile` feature and add the world-scoped `FeatureSet` resolution API, then thread `&FeatureSet` through boot planning and ingress extraction. This is the evaluation core that `M3.69.2` (CLI/daemon) consumes.

# Contract (exact public names; M3.69.2 is written against these)

`crates/slipfork-forkfile`:
- `pub enum FeatureEdit { Enable(String), Disable(String) }` with `FromStr` (`feat`, `+feat`, `-feat`, `!feat`) + `Display` (`+feat`/`-feat`) + serde.
- `FeatureSet` gains `contains(&str)`, `available() -> &BTreeSet<String>`, `pub fn from_active(iter) -> Self` for tests.
- `Forkfile::available_features()` = `{ "dev" } ∪ every features group name ∪ every feature listed inside a group`.
- `Forkfile::resolve_features(&self, no_default: bool, edits: &[FeatureEdit]) -> Result<FeatureSet, Diagnostic>`: base = (unless `no_default`) `{ "dev" } ∪ expand("default")`; `dev` expands its declared implications (`features { dev = [mail] }` -> mail). Apply edits in order; enabling expands transitively (cycle-safe); disabling removes that name only. Unknown name => 1-line `unknown feature `x`` + `next: use one of dev, mail, ...` at the features span.
- `Forkfile::default_features() -> FeatureSet` (== `resolve_features(false, &[])`, error-free).
- `matches()` drops the `declared` escape hatch: `ExprKind::Feature(name) => self.active.contains(name)`.
- Feature-aware variants (old signatures that silently used default features are **replaced**; all in-tree call sites updated): `app_domains_with(&FeatureSet)`, `primary_domain_with`, `eval_image_args_with(..., &FeatureSet)`, `eval_image_stage_with(..., &FeatureSet)`, `eval_all_env_with(world, scheme, resolvers, &FeatureSet)`, `services_with`, `resources_with`, `domains_with`, `env_bindings_with`.
- `crates/slipfork-forkfile/src/boot.rs:263` `boot_plan_with(doc, root, &FeatureSet)`; `boot_plan(doc, root)` delegates to `default_features()`.

`crates/slipfork-world`: `world_plan_with(doc, root, &FeatureSet)` (plan.rs:13) and `graph::finish` takes the set; `world_plan` delegates to default.

`crates/slipfork-ingress`: `IngressTable::from_forkfile_with(world, doc, &FeatureSet)` (extract.rs:28); `from_forkfile` delegates.

`crates/slipfork-lsp`: `dev` is recognized (completions/hover, no unknown-feature diagnostic) even with zero `features {}` blocks; `!dev` works.

# Anchors

- `crates/slipfork-forkfile/src/eval/mod.rs:31-56` (FeatureSet/matches), `:67-86` (active_features), `:88-179` (all eval entry points).
- `crates/slipfork-forkfile/src/visit/mod.rs:76-130` (services/resources/domains/env_bindings flatten `on` branches without feature filtering — must learn `&FeatureSet`).
- `crates/slipfork-forkfile/src/check/mod.rs:148` `declared_features` — reuse for `available_features` rather than re-scanning.
- `crates/slipfork-forkfile/src/boot.rs:263`, `crates/slipfork-world/src/boot/plan.rs:13`, `crates/slipfork-world/src/boot/graph.rs:60`, `crates/slipfork-ingress/src/route/extract.rs:28`.
- `crates/slipfork-forkfile/tests/feature_gating.rs`, `crates/slipfork-forkfile/src/tests/commands.rs`, `crates/slipfork-ingress/tests/route_table_journey.rs` — update to new semantics (bare `on mail` is false unless active).

# Acceptance

- `cargo test -p slipfork-forkfile -p slipfork-world -p slipfork-ingress -p slipfork-lsp` exits 0, including unit tests for: built-in `dev` with zero `features {}`; `features { dev = [mail] }` implication; no-default-features (empty base); `+mail`, `-dev`, `!mail`; unknown-feature diagnostic remedy; a mail-gated service absent from `services_with` when mail is off.
- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` exits 0.
- Do **not** touch `crates/slipfork/**`, `support/**`, `specs/**`, `docs/**`, `README.md`, or `packaging/man/**`.
