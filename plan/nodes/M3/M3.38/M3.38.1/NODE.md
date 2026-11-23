---
id: M3.38.1
title: "Clean-sheet Rust 2024 workspace, Makefile, minimal slipfork binary & ARCHITECTURE.md"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - ARCHITECTURE.md
  - "crates/slipfork/**"
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-image/**"
  - "crates/slipfork-ingress/**"
  - "crates/slipfork-ospp/**"
  - "crates/slipfork-vcs/**"
  - "crates/slipfork-lsp/**"
  - rustfmt.toml
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T00:58"
updated: "2026-11-21T01:02"
---

# Goal
Scaffold the clean-sheet Rust 2024 workspace: root `Cargo.toml` with exactly the eight canonical members (`slipfork`, `slipfork-forkfile`, `slipfork-world`, `slipfork-image`, `slipfork-ingress`, `slipfork-ospp`, `slipfork-vcs`, `slipfork-lsp`), a `Makefile` with `check`/`fmt`/`clippy`/`test` targets, a minimal `slipfork` binary with a vaporware-free `--help`/`--version`, and `ARCHITECTURE.md` that mirrors `specs/03` exactly.

# Context
- Crate ownership + the six canonical ports (`WorldEngine`, `BaseImageBuilder`, `SecretVault`, `DaemonControl`, `OsppClient`, `VcsWorkspace`): `specs/03-under-the-hood.md:116` (section `## 5. Crate & Port Map`).
- Single Composition Root rule: only `crates/slipfork/src/main.rs` reads `std::env`, probes `/proc`, or binds ambient sockets; `slipfork daemon` is a mode of the same binary (`specs/03-under-the-hood.md:116-120`).
- `daemon.sock` NEWLINE-JSON control plane (`{"v":1,...}`, `SCM_RIGHTS` only for `ingress.accept`): `specs/03-under-the-hood.md:132-145`.
- Ingress ownership of `:80` framing + route matching with an injected `TcpListener`: `specs/03-under-the-hood.md:116-124` and `:146-152`.
- Repo currently holds only `specs/`, `tools/`, `plan/`, `TESTING_PLAN.md`, `AGENTS.md`, `rustfmt.toml`: create every member crate directory here. No `slipfork-engine`, `slipfork-edge`, `slipfork-snow`, or `slipfork-testkit` may appear.
- `ARCHITECTURE.md` must state: one composition root, the six-port -> adapter table, `daemon.sock` request/response frames, stateless ingress forwarding one accepted fd to the daemon, and that no second control socket exists.

# Acceptance
- `cargo build --workspace` succeeds (edition 2024); `ls crates` lists exactly the eight members.
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo run -p slipfork -- --help` and `cargo run -p slipfork -- --version` print only shipped surface (zero `[derived]`, `Unavailable*`, or invented subcommands).
- `ARCHITECTURE.md` is `<= 200` lines, uses Mermaid + `!!!` admonitions where useful, bans `ADR-*`/`M3.*` lawyer prose, and reproduces the `specs/03` §5 crate/port names verbatim.
- `uv run --no-project tools/guard.py`
