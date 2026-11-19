---
id: M3.30.4.3
title: "Journey B: one rich OSPP v1 loopback E2E suite + pure algorithm tests"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.30.4.1]
paths:
  - "crates/slipfork/tests/**"
  - "crates/slipfork-ospp/**"
  - "crates/slipfork-ospp-testkit/**"
  - Makefile
  - tools/do_vm.py
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T02:19"
updated: "2026-11-19T02:39"
---

# Goal
Section 2 Journey B of `plan/nodes/M3/M3.30/M3.30.4/NODE.md`.

# Work
- Collapse the 11 OSPP daemon boots into ONE loopback reference-server journey: `auth <url>` `#handle=` pairing -> `<state_dir>/ospp.json` `0600` -> `slipforkd` reboot with no `--ospp-endpoint` -> encrypted subscribe with local-vault claims -> assert zero secret bytes on disk -> live `env_updated` restarts `api` -> streaming HPKE dump restore.
- Keep pure algorithm tests (HPKE/wire framing, template taint, DAG waves, ANSI width math) separate and fast.

# Constraints
Real `127.0.0.1:0` reference server only; zero mocks; no assertion weakened; no RAM `/tmp` for state. No RESULT.md.

# Acceptance
- `cargo test -p slipfork-ospp`; `cargo test -p slipfork --test critical_ospp_daemon` (fail closed off-Btrfs is expected and reported)
- `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`
