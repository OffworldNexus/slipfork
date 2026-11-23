---
id: M3.69
title: "Built-in dev feature, per-world CLI features (-F/--features, --no-default-features, slipfork features, fork inheritance) & dev-only /project bind-mount"
kind: epic
status: done
priority: 3
depends_on: []
paths:
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-ingress/**"
  - "crates/slipfork-lsp/**"
  - "crates/slipfork/**"
  - "packaging/man/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T13:07"
updated: "2026-11-23T13:43"
---

# Goal

Implement first-class per-world `features` across the compiler, CLI, daemon, and pod runtime, plus dev-only `/project` host bind-mounting so `!dev` runs pure immutable `Dockerfile` builds (`ENTRYPOINT`/`CMD`):

## 1. Built-in `dev` Feature (`crates/slipfork-forkfile` & `crates/slipfork-lsp`)

- `dev` is a **built-in feature** implicitly defined in every `Forkfile` (even when no `features { ... }` block exists, or when `features { mail }` doesn't list `dev`). Authors MAY still write `dev = [mail]` inside `features { ... }` to make `dev` imply additional features, or `default = [...]` for extra default features.
- By default, a world's base feature set is `{ "dev" } ∪ features.default` (expanded transitively through feature groups).
- Add an explicit feature-evaluation API on `Forkfile` (e.g. `resolve_features(no_default: bool, edits: &[FeatureEdit]) -> Result<FeatureSet, Diagnostic>` where `FeatureEdit` is `Enable(String)` / `Disable(String)`, validating every named feature against `{ "dev" } ∪ declared_features` with a sharp `unknown feature` error + `next:` listing valid features!), and thread `&FeatureSet` into `boot_plan_with_features`, `eval_all_env_with_features`, `eval_image_args_with_features`, `eval_image_stage_with_features`, `app_domains_with_features`, `services_with_features`, etc. (replacing hardcoded `self.features()` callers in `crates/slipfork` so the world's active `FeatureSet` drives EVERY evaluation!).
- Update `crates/slipfork-lsp` so `dev` is recognized in completions/hover even without an explicit `features { dev }` entry.

## 2. Per-World Persisted Feature State & Fork Inheritance (`crates/slipfork`)

- Store each world's feature state in `<project_checkout>/.slipfork/features.json` (`{ "no_default": bool, "edits": ["+mail", "-dev", ...] }` or canonical active list/overrides).
  - When `<project>/.slipfork/features.json` does not exist, the world uses the default feature set (`dev` + `features.default`, expanded).
  - Because `slipfork fork <name>` snapshots `<project>` -> `<project>/.forks/<name>` via Btrfs CoW, **forks automatically inherit their parent world's `.slipfork/features.json`** at fork time, and subsequent feature changes in the fork (`cd .forks/<name> && ...` or targeting the fork) mutate only that fork's `.slipfork/features.json`!
- **CLI Surface (`crates/slipfork/src/cli/verbs.rs`):**
  - On `up` and `fork`: add `-F, --features <FEATURES>` (comma-separated `feat`, `+feat`, `-feat` / `!feat`; allow `-F -dev` via `allow_hyphen_values = true`!) and `--no-default-features`.
    - Passing `-F` or `--no-default-features` on `up` updates & persists that world's `.slipfork/features.json` and reconciles the world (rebuilding images if target stage/args changed, starting newly enabled pods, reaping newly orphaned/disabled pods like `mailpit` when `-F -mail`!).
    - Passing `-F` or `--no-default-features` on `fork` applies on top of the inherited parent world's features and persists into `<fork_checkout>/.slipfork/features.json` before booting the fork!
  - On `env` and `validate`: add `-F, --features <FEATURES>` and `--no-default-features` (evaluates with those overrides without mutating `.slipfork/features.json`; when omitted, uses the world's persisted `.slipfork/features.json`!).
  - Add verb **`slipfork features`** (`[TARGET]` or subcommands / modifiers):
    - `slipfork features` (no modifiers): prints every feature (`dev` + declared features) via borderless `ui::table` showing `● <name>  active` / `○ <name>  inactive` (and any group implications like `-> mail`), plus `--json`.
    - `slipfork features enable <FEAT>...`, `slipfork features disable <FEAT>...`, `slipfork features reset` (or `slipfork features +mail -dev`): validates & persists into `.slipfork/features.json` (and prints `next: run 'slipfork up' to apply to the running world` if the world is currently up).
  - Ensure `ps`, `status`, `daemon`, and `__supervise` all evaluate the `Forkfile` using that world's `<world_checkout>/.slipfork/features.json` so `ps`, `ingress` routes, `env`, and `up` ALWAYS agree!

## 3. Dev-Only `/project` Bind-Mount & Pure Docker `!dev` Behavior (`crates/slipfork/src/world/runtime.rs`)

- Currently `crates/slipfork/src/world/runtime.rs:426 & 491` unconditionally bind-mounts `project_bind(&self.project)` onto `/project` even when `dev` is disabled (`!dev`)!
- Fix: bind-mount `project_bind(&self.project)` **ONLY when the world's active `FeatureSet` contains `"dev"`** (`if self.features.contains("dev") { vec![project_bind(&self.project)] } else { Vec::new() }`)!
  - When `dev` is active: `/project` is bind-mounted for live reload (`vite dev`, `uvicorn --reload`, `@uv` / `@pnpm`).
  - When `dev` is disabled (`-F -dev` / `--no-default-features`): pods run purely against the built container rootfs (`WORKDIR`, `COPY`, `RUN`, `ENTRYPOINT`/`CMD` from `Dockerfile`) with zero host `/project` bind-mount shadowing built image files — exact Docker behavior!

# Acceptance

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` exits `0`.
- `cargo test -p slipfork-forkfile -p slipfork-lsp -p slipfork` exits `0` (including tests for built-in `dev`, `-F -dev` / `+mail`, fork feature inheritance, `slipfork features`, and `!dev` omitting `/project` bind).
