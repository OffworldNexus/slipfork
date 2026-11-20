---
id: M3.35.2
title: "Prune all non-§7 inline cfg(test) modules; keep tight §7 focused suites"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.35.1]
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork-engine/src/**"
  - "crates/slipfork-world/src/**"
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork-vcs/src/**"
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-wire/**"
  - "crates/slipfork-ospp/**"
  - "crates/slipfork-lsp/**"
  - "crates/slipfork-dag/**"
  - "editors/**"
  - "tools/tests/forkfile-highlight.test.js"
  - Makefile
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-20T16:14"
updated: "2026-11-21T00:43"
---

# Goal

Enforce `TESTING_PLAN.md` §1: delete every non-§7 inline `#[cfg(test)]` module, `src/**/tests*` directory and micro-component suite, keeping only tight focused suites for §7 (Forkfile language, wire/OSPP framing, LSP/Zed, highlighter). Do not write replacement unit tests.

# Context

- **Delete (non-§7) all inline `#[cfg(test)]` bodies** in these 62 files: `crates/slipfork/src/**` (27: `apparmor.rs`, `cli.rs`, `commands/{boot,interact,migrate,process,secret}.rs`, `daemon/{mod,protocol,resolve}.rs`, `daemon_unit_install.rs`, `doctor/mod.rs`, `hook_failure.rs`, `layout.rs`, `logs/{mod,renderer}.rs`, `open.rs`, `project.rs`, `ps.rs`, `restart.rs`, `status/{lineage,mod,table}.rs`, `ui.rs`, `user.rs`, `vaults.rs`, `vcs.rs`), `crates/slipfork-engine/src/**` (21: `btrfs.rs`, `btrfs_engine/pod.rs`, `identity.rs`, `lib.rs`, `lifecycle.rs`, `linux_cow.rs`, `oci.rs`, `rootfs/{abi,builder,buildx,cache,contract,dedupe,dockerfile,input,mod,pin}.rs`, `stream.rs`, `supervise.rs`, `wait.rs`, `world_init.rs`), `crates/slipfork-world/src/**` (8: `env,format,logs,merkle,pod,port/values,socket,weaver`), `crates/slipfork-edge/src/**` (3: `registry.rs`, `registry/ingress_table.rs`, `unit.rs`), `crates/slipfork-vcs/src/**` (3: `jj.rs`, `ports.rs`, `tool_command.rs`), plus `crates/slipfork-testkit/src/{lib,pods}.rs` and `crates/slipfork-ospp-testkit/src/reference_server/claims.rs`.
- **Delete the whole `crates/slipfork/src/daemon/tests/` directory** (`mod.rs`, `policy.rs`, `refusals.rs`). Its unique fail-closed/secret-taint assertions must reappear as real E2E story checks in M3.35.3 (`e2e_ospp_edge.rs`, `e2e_cli.rs`) — list every behaviour you delete in your final message so M3.35.3 cannot silently drop it.
- **Keep + tighten §7** (`TESTING_PLAN.md` §7). Keepers: `crates/slipfork-forkfile/src/{argv,check,diag,eval,fmt,hover,ir,lexer,migrate/mod,migrate/validate,parser,resolve/mod,vault,yaml}.rs` (syntax, multi-error checker with `line:col` carets, secret taint, idempotent comment-preserving fmt, `snow.yml` migrate); `crates/slipfork-wire/src/control/frame.rs` (local daemon RPC framing); `crates/slipfork-ospp/src/{client,endpoint,keys,model,pairing,wire}.rs` (OSPP v1 WS + `hpke-p256`); `crates/slipfork-lsp/src/{framing,server,utf16}.rs` (UTF-16 diagnostics/hover/format). Delete any §7 inline test that duplicates an integration test — keep exactly one home per assertion.
- §7 integration budget: `crates/slipfork-forkfile/tests/` -> `forkfile_language.rs` + `forkfile_docs.rs` (render every ```forkfile fence in `README.md` and `docs/**` through parse+fmt+check, including `critical_migrate.rs` coverage) <= 1,500 LOC total, deleting `critical_evaluated_world.rs`/`critical_forkfile.rs`/`demo_forkfile.rs` fragmentation; `crates/slipfork-ospp/tests/critical_ospp.rs` <= 900 LOC; `crates/slipfork-lsp/tests/` -> `lsp_stdio.rs` <= 600 LOC driven over real stdio JSON-RPC; `slipfork-wire` inline tests <= 400 LOC.
- Keep `make zed-check` (`editors/tree-sitter-forkfile` corpus + `editors/zed` WASM build) and `tools/tests/forkfile-highlight.test.js` behaviourally intact.

# Acceptance

- `! test -d crates/slipfork/src/daemon/tests && test -z "$(grep -rl 'cfg(test)' crates/slipfork/src crates/slipfork-engine/src crates/slipfork-world/src crates/slipfork-edge/src crates/slipfork-vcs/src crates/slipfork-testkit/src crates/slipfork-ospp-testkit/src)"`
- `cargo test --workspace --lib --bins` green (only §7 lib suites remain)
- `cargo test -p slipfork-forkfile -p slipfork-ospp -p slipfork-lsp -p slipfork-wire --tests`
- `cargo clippy --workspace --all-targets -- -D warnings && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`
- `make zed-check && bun test tools/tests/forkfile-highlight.test.js`
- `cat crates/slipfork-forkfile/tests/*.rs | wc -l` <= 1500
