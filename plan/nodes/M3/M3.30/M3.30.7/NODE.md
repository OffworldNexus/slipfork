---
id: M3.30.7
title: "Lab-VM reality fixes: short test root, browser fixture on CLI, real seal invariant, stale-command pruning"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.30.4]
paths:
  - "crates/slipfork-engine/tests/**"
  - "crates/slipfork-testkit/**"
  - "crates/slipfork/tests/**"
  - "crates/slipfork-edge/tests/**"
  - tools/do_vm.py
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-19T03:26"
updated: "2026-11-19T04:28"
---

# Goal
Make the consolidated live suites genuinely green as the suite user on 165.232.86.174 (M3.30.6 blocker).

# Verified failures (suite-user envelope, HOME=/var/lib/slipfork/home)
- `crates/slipfork-engine/tests/critical_engine.rs:270-282`: asserts the sealed snapshot root mode has no owner-write bit (0o200). That was a `TestTreeDriver` detail (deleted in M3.30.1.3.1); a real Btrfs snapshot keeps 0o755 and seals with BTRFS_SUBVOL_RDONLY (`ro=true`). Replace with the real invariant: writing a new entry AND removing/mutating an existing payload file both fail.
- AF_UNIX 107-byte limit: `$HOME/.cache/slipfork/test-tmp/engine/<hash>-<pid>/state/ctl/<world>/netns.sock` overflows for long world ids (e.g. `fork-no-images-2`) under `/var/lib/slipfork/home` (critical_oci_runtime, and the new testkit `WorldFixture`). Provide a short disk-backed root: `SLIPFORK_TEST_ROOT` env honored by the three `disk_scratch_root()` helpers, and `tools/do_vm.py` sets it (for example `/var/lib/slipfork/t`, created + chowned on the Btrfs volume).
- `critical_cli.rs` `support_json_lists_required_evidence_for_every_integration`: `slipfork support` no longer exists (deleted in M3.30.1.2) -> delete the test.
- auth opener (`critical_cli.rs:1319`, `critical_ospp_daemon.rs:378`): on the Btrfs `Backend::Child` the browser fixture is only on the *daemon* PATH; the CLI opens the browser and cannot find the fixture -> `browser-argv.txt` missing. Put the fixture bin on the CLI command PATH too (prepend, keep the host PATH).
- `critical_cli.rs:845` zero-setup teardown: `kill -TERM -<pgid>` returns ESRCH once the group is already empty; treat "no live members" as the proof instead of demanding success.
- `critical_l0_wiring.rs:216`: `--host-caches` mounts come from the daemon HOME; `/var/lib/slipfork/home` has no `.cargo`/`.rustup`, so the ABI probe never runs. Give the daemon a harness HOME that carries at least one toolchain cache (or create the witness cache in the fixture).
- `critical_world_journey.rs:418`: `open` under an empty PATH fails on missing `git` before the opener remedy. Use a restricted PATH that still exposes the real required tools but no opener.

# Acceptance
- `tools/do_vm.py sync && tools/do_vm.py test` exits 0 as the suite user.
- fmt/clippy; `cargo test --workspace --tests --no-run`; `rs_test_list`.
- No assertions weakened; every replaced assertion names the real substrate invariant.
