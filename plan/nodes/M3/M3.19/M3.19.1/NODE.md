---
id: M3.19.1
title: "Apply the H-006 AppArmor attachment set: profile covers every CLONE_NEWUSER executable"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.18]
paths:
  - crates/slipfork/src/apparmor.rs
  - crates/slipfork/src/doctor.rs
  - "crates/slipfork-engine/src/lib.rs"
  - tools/do_vm.py
  - tools/tests/test_do_vm.py
  - docs/reference/cli.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-17T13:37"
updated: "2026-11-17T13:42"
---

# Goal

Apply the architect's H-006 ruling: the AppArmor `userns` profile must attach every executable that performs `CLONE_NEWUSER` for a world, not only `<bin_dir>/slipfork{,d}` — otherwise Ubuntu 24.04 transitions the in-world `/.slipfork/bin/crun` into `unprivileged_userns` on `execve` and every world boot dies with `unshare: Operation not permitted`.

# Context

`docs/decisions/ADR-0010-btrfs-only-world-engine.md` §7 (corrected) and `docs/design/do-vm-harness.md` carry the ruling. Live evidence is in `plan/nodes/M3/M3.19/RESULT.md` §2-§3, §10.

# Design / Invariants

1. `crates/slipfork/src/apparmor.rs::render_userns_profile` renders one leading-slash brace expression:
   `profile slipfork /{<rel_bin_dir>/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} flags=(unconfined) {` + `  userns,` + `}` (bare `slipfork{,d,-world-init}` when `bin_dir == "/"`). Zero `//` sequences; every render carries a unit test.
2. `crates/slipfork-engine/src/lib.rs::userns_remediation_hint` still names `slipfork daemon unit --only apparmor` + `apparmor_parser -r`; no claim that the entry binary alone suffices.
3. `tools/do_vm.py::render_apparmor_body` emits the harness clause `/usr/local/bin/slipfork{,d,-world-init},{rel}/target/{debug,release}/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun` where `rel = remote_tree.strip("/")`.
4. `tools/tests/test_do_vm.py`: exact 4-line output on each side, full-profile byte-equality after normalising ONLY the `<bin_dir>` clause to `<bin_clause>`, with `,usr/bin/unshare,.slipfork/bin/crun}` byte-for-byte identical; the "no host crun" provisioning test scopes to `PACKAGES`/`apt-get install` rather than a bare `crun` substring.
5. `docs/reference/cli.md` shows the rendered profile verbatim.

# Acceptance

```bash
cargo test -p slipfork --lib apparmor
cargo run -q -p slipfork -- daemon unit --only apparmor
! grep -n '//' <(cargo run -q -p slipfork -- daemon unit --only apparmor)
uv run --no-project --with pytest pytest -q tools/tests/test_do_vm.py
uvx --offline ruff check tools && uvx --offline ruff format --check tools
cargo clippy --workspace --all-targets -- -D warnings
make docs-check
```
