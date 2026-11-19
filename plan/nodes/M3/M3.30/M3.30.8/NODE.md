---
id: M3.30.8
title: Never write the world component env into an OCI container config.json (secret plaintext on disk)
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M3.30.4]
paths:
  - "crates/slipfork-engine/src/btrfs_engine/**"
  - "crates/slipfork-engine/tests/**"
  - "crates/slipfork-world/src/port/values.rs"
  - ARCHITECTURE.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-19T04:28"
updated: "2026-11-19T04:59"
---

# Goal
Fix the real leak the consolidated OSPP journey found: a backing-service OCI `config.json` carries the world component environment, including resolved secrets.

# Evidence (suite-user VM, e0e9aee)
`critical_ospp_daemon` fails `remote secret plaintext leaked onto disk` with hits:
- `<state>/ctl/canonical-2/oci/sf2-pg_clusters_main/config.json`
- `<state>/ctl/canonical-2/oci-run/sf2-pg_clusters_main/config.json`
Its `process.env` contains `DEBUG=local-debug`, `LOCAL_PASSWORD=local-password`, `REMOTE_TOKEN=remote-token-…`, `REMOTE_VAR`, `WORKER_VAR` plus the pg vars. Root cause: `crates/slipfork-engine/src/btrfs_engine/services.rs:201` builds every service env as `child_env::materialize(&record.base_env, &spec.env, inherited)`; `record.base_env` is the world component env (all `modes.*.envs.*`, secrets included), and `spawn_oci_container` writes that env verbatim into the bundle `config.json`.

# Fix
In `start_service`, choose the base environment by runtime kind: `ServiceRuntime::OciContainer` materializes against an EMPTY base env (inherited allowlist + `spec.env` only); `ServiceRuntime::WorldProcess` keeps `record.base_env` (components and hooks need the world env). Add a short comment stating the invariant: an OCI container bundle must never carry the world component env. Check `update_service_env`/`restart_with_env` for the same path and keep backing services clean.

# Acceptance
- New engine assertion (e.g. in `critical_oci_runtime.rs`): a container spawn over a world `base_env` containing a marked secret writes no marker into `ctl/<world>/oci*/**/config.json`, while a `WorldProcess` service still receives the world env.
- `cargo test -p slipfork-engine --lib`; `cargo test --workspace --tests --no-run`; fmt; clippy -D warnings.
- On the VM: `critical_oci_runtime`, `critical_engine`, and `critical_ospp_daemon` exit 0.
