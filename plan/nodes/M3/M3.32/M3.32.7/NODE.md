---
id: M3.32.7
title: "slipfork-forkfile: lossless snow.yml -> Forkfile converter (upstream Kerfufoo schema, ${{ }} templates, self-verified)"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.32.1]
paths: ["crates/slipfork-forkfile/**", "crates/slipfork-snow/src/**", Makefile, Cargo.toml, Cargo.lock]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T16:00"
updated: "2026-11-19T19:03"
---

# Goal

Add `slipfork_forkfile::migrate_snow_yaml(&str) -> Result<String, MigrateError>`: parse a REAL upstream Kerfufoo `snow.yml` and emit an idiomatic, statically valid `Forkfile`.

# Context

OWNER ADDITION to `M3.32` ("include a snow.yml to Forkfile conversion tool in the mix, actually"). Must land BEFORE `M3.32.3` deletes `crates/slipfork-snow` (its parser is the reference), so `crates/slipfork-snow/src/{model.rs,faceless.rs}` is temporarily readable here.

Input schema (top level): `images:` (`BuildImage {id, context, dockerfile, envs}`, `DockerHub {id, registry, repository, tag}`), `resources:` (`pg_clusters`, `pg_databases {id, cluster, pool}`, `redis_clusters`, optional `s3_buckets`), `domains:`, `envs: { <group>: { KEY: value } }` plus legacy `modes:` when present, `components: [{id, image, run_command, size_hint, envs, internal_ports}]`, `ingresses: [{match:{prefix}, target:{component, port}}]`, `hooks: [{component, when, run_command}]`.

Template translation (Faceless `${{ ... }}` -> `Forkfile`): `secrets::KEY` -> `secret(KEY)` and `vars::KEY` -> `var(KEY)`, both preserving `?? "default"`; `resources::pg_databases.<db>.<prop>` / `resources::redis_clusters.<id>.<prop>` / `domains::<id>.<prop>` / `components::<id>.<prop>` / `resources::app.<prop>` -> `<id>.<prop>` / `app.<prop>`; `deployment::<path>` -> `deployment.<path>`; mixed string templates become `+` concatenations (`"https://" + main.name + "/cb?k=" + secret(K)`).

Emit: `postgres <cluster> { version ...; database <db> ... }`, `redis <id>`, `domain <id>`, `env <group> { ... }`, `service <id> <./ctx | from reg/repo:tag> { port ...; route ...; use env ...; pre-deploy ...; run ... }`.

HARD INVARIANT: `migrate_snow_yaml` runs `parse_forkfile(&out)` before returning, so every emitted file is guaranteed statically valid. Runtime still reads ONLY `Forkfile`; migrate is a conversion tool, never a second runtime format.

# Acceptance

- `cargo test -p slipfork-forkfile` exits 0 with fixtures for a realistic multi-service upstream file (`api` + `front` + `worker` + pg cluster/dbs + redis + domains + mixed `${{ ... ?? ... }}` + hooks + ingresses).
- Round-trip proof: migrate -> `parse_forkfile` -> `evaluate` succeeds and services/ports/routes/env groups match the source intent.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` exit 0.
- Write NO `RESULT.md`.
