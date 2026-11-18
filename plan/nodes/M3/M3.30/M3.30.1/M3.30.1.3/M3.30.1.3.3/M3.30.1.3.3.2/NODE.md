---
id: M3.30.1.3.3.2
title: "Concrete host/ingress probes: delete HostProbe/IngressProbe traits and recorders"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.1.3.3.1]
paths:
  - Cargo.toml
  - Cargo.lock
  - crates/slipfork/src/doctor.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/bin/slipforkd.rs
  - "crates/slipfork/tests/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-18T23:13"
updated: "2026-11-18T23:23"
---

# Goal
Split 2/4 of `plan/nodes/M3/M3.30/M3.30.1/M3.30.1.3/M3.30.1.3.3/NODE.md`: probes become concrete.

# Work
- `crates/slipfork/src/doctor.rs`: delete `HostProbe`/`FakeHostProbe` and `IngressProbe`/`RecordingIngressProbe`; one concrete probe struct each reading real `/proc`, sysfs, socket ownership and `systemctl` state.
- Fold the doctor tests onto real observed values (accept either branch of host-dependent facts), keep `✓ Ready` / `✗ N issue(s)` verdict-first output and the `next:` remedies byte-identical.
- Wire the concrete probes in the composition roots only.

# Constraints
No traits/recorders/mocks; composition-root-only ambient reads; no ANSI change. No RESULT.md.

# Acceptance
- `! grep -rEn "FakeHostProbe|RecordingIngressProbe|trait HostProbe|trait IngressProbe" crates/`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib`
