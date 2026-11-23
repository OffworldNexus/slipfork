---
id: M3.69.2
title: "Per-world features.json, -F/--features CLI, slipfork features verb & dev-only /project bind"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.69.1]
paths: ["crates/slipfork/**", "packaging/man/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T13:10"
updated: "2026-11-23T13:11"
---

# Goal

Give every world a persisted, fork-inherited feature configuration, expose it on the CLI, and make `/project` a dev-only bind mount. Consumes the `M3.69.1` feature API.

# 1. Persistence (`crates/slipfork/src/world/features.rs`)

- `pub struct WorldFeatures { pub no_default: bool, pub edits: Vec<FeatureEdit> }` (serde) stored at `<project>/.slipfork/features.json` as `{"version":1,"no_default":false,"edits":["+mail","-dev"]}`.
- `WorldFeatures::load(project) -> Self` (absent file = default, no error), `save(project)` atomic (`tmp` + rename), `resolve(&self, doc) -> Result<FeatureSet, Diagnostic>`, `apply(&mut self, edits)`, `Display` of edits.
- Forks inherit automatically because `.slipfork/features.json` lives inside the snapshotted `<project>` checkout.

# 2. Evaluation must use the world's features everywhere in `crates/slipfork`

- Daemon lifecycle (`daemon/lifecycle/up.rs`, `plan.rs:24`, `core.rs:225`, `view.rs`, `logs.rs:630`), supervisor (`world/up.rs`, `world/supervise.rs:51`), `open.rs:109`, `commands/env.rs`, `ps`/`status` rows: resolve `WorldFeatures::load(project)` -> `FeatureSet` and use `world_plan_with`, `IngressTable::from_forkfile_with`, `services_with`, `eval_all_env_with(..., &features)`, `app_domains_with`, `eval_image_args_with`, `eval_image_stage_with`.
- `daemon/lifecycle/up.rs:323` `fingerprint()` must hash the resolved active feature list so a features edit forces reconcile (registry already has `features: Vec<String>`).
- `__supervise` resolves from `<project>/.slipfork/features.json` too; every viewer agrees with `up`.

# 3. CLI (`cli/verbs.rs`, `cli/dispatch.rs`, `world/up.rs`, `commands/*`)

- `-F, --features <FEATURES>` on `up`, `fork`, `env`, `validate`, `features` (`value_delimiter = ','`, `allow_hyphen_values = true` so `-F -dev` parses; bare `mail` = enable, `+mail`/`-mail`/`!mail` = edits) plus `--no-default-features` on the same verbs.
- `up`: when any flag is typed, resolve against the compiled `Forkfile`, persist to `<project>/.slipfork/features.json`, then boot; the daemon re-plans (build newly selected stages/args, start newly enabled pods, reap pods/services gone inactive such as `mailpit` on `-F -mail`).
- `fork`: pass `no_default_features` + edits in the `fork` request; after the snapshot, the daemon writes them into `<fork_checkout>/.slipfork/features.json` before booting (on top of the inherited file).
- `env` / `validate`: `-F`/`--no-default-features` evaluate non-destructively (never write the file); omitted = persisted file.
- New verb `slipfork validate [TARGET]`: compile + resolve features, print `✓ Forkfile valid` plus an active-features line (and `-v` plan rows) via `ui::table`; `--json` emits `{"valid":true,"features":[...],"images":[...],"services":[...]}`; diagnostics exit 1 as `1 line + next:`.
- New verb `slipfork features`: no subcommand = borderless `ui::table` of every feature (`dev` + declared) with `● name active` / `○ name inactive` and implications (`dev -> mail`), honoring `NO_COLOR`/`--json`. `features enable <f>...`, `features disable <f>...`, `features reset`, and `features +mail -dev` persist into `.slipfork/features.json` (validate first; print `next: run 'slipfork up' to apply to the running world` when the world is up).

# 4. Dev-only `/project` bind (`crates/slipfork/src/world/runtime.rs:426,491`)

- `WorldRuntime` carries the resolved `FeatureSet`; both `PodSpec.binds` sites use `project_bind(&self.project)` **only when** `features.contains("dev")`, else `Vec::new()`. With `-F -dev`/`--no-default-features` pods run purely against the built rootfs (`WORKDIR`/`COPY`/`RUN`/`ENTRYPOINT`/`CMD`). Add a pure helper with an algorithm unit test (e.g. `fn dev_binds(features, project) -> Vec<Bind>`).

# 5. Surface sync + tests

- Update `crates/slipfork/tests/critical_cli_surface.rs` (verb list/help) and regenerate `packaging/man/slipfork.1` (`cargo run -q -p slipfork -- --manpage > packaging/man/slipfork.1`) so `crates/slipfork/tests/manpage_drift.rs` passes.
- Consolidate real lab coverage: extend `crates/slipfork/tests/critical_host_verbs.rs` (real `/var/tmp`, real daemon socket) for `features`, `features enable/disable/reset`, `validate`, `env -F`, `-F -dev` non-destructive evaluation, and bad-feature remedy; add focused unit tests for `WorldFeatures` round-trip and `FeatureEdit` parsing. No mocks.

# Acceptance

- `cargo test -p slipfork` exits 0; `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` exits 0.
- `slipfork --help` shows both `features` and `validate`; `packaging/man/slipfork.1` matches `--manpage` byte-for-byte.
- `-F -dev` parses as a value (never as a flag).
