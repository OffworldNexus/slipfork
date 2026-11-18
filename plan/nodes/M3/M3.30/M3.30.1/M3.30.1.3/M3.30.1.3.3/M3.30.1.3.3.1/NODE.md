---
id: M3.30.1.3.3.1
title: "Concrete user identity: delete UserIdentity/HostIdentity traits and FakeIdentity"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths:
  - Cargo.toml
  - Cargo.lock
  - crates/slipfork/src/user.rs
  - crates/slipfork/src/daemon_unit_install.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/bin/slipforkd.rs
  - "crates/slipfork/tests/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-18T23:13"
updated: "2026-11-18T23:17"
---

# Goal
Split 1/4 of `plan/nodes/M3/M3.30/M3.30.1/M3.30.1.3/M3.30.1.3.3/NODE.md`: identity becomes a concrete value.

# Work
- `crates/slipfork/src/user.rs`: replace `UserIdentity`/`HostIdentity` traits + `FakeIdentity` with one concrete `UserIdentity { uid, gid, home, account }` value built only in `crates/slipfork/src/main.rs` (and `bin/slipforkd.rs` where the daemon needs it).
- `target_account`/`install` take `&UserIdentity`; `lookup_account` reads the real host passwd.
- Rewrite the `daemon_unit_install` unit tests onto real host accounts (assert real uid/gid/home; the refusal branches use a truly unknown account name) — no fake identity, no `#[cfg(test)]` hatch.

# Constraints
Composition-root-only env/proc reads; no traits; no aliases; no mocks. No RESULT.md.

# Acceptance
- `! grep -rEn "FakeIdentity|trait UserIdentity|trait HostIdentity" crates/`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib`
- `cargo test -p slipfork --test critical_cli` (off-Btrfs: must fail closed with the existing one-line Btrfs message)
