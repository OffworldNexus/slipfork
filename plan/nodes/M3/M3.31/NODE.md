---
id: M3.31
title: "0.1.0 gate polish: purge --help DAG vaporware, align doctor transcripts & encapsulate snow expression check"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30]
paths:
  - crates/slipfork/Cargo.toml
  - crates/slipfork/src/cli.rs
  - crates/slipfork/src/daemon/resolve.rs
  - "crates/slipfork-snow/src/**"
  - "docs/getting-started.md"
  - docs/reference/cli.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T07:00"
updated: "2026-11-19T07:00"
---

# Goal

Close the three user-facing `0.1.0` honesty & canonical-module defects caught by the `M3` Milestone Triad Gate in one surgical pass (no Btrfs VM re-run needed; pure Rust + Zensical docs):

1. **Purge `0.2.0` test-DAG vaporware from `0.1.0` CLI `--help` and crate metadata:**
   - `crates/slipfork/src/cli.rs:32`: change `about = "Instant CoW dev worlds & parallel test DAG orchestrator"` to `about = "Instant CoW Btrfs dev worlds & worktree forks from snow.yml"`.
   - `crates/slipfork/Cargo.toml:3`: change `description` from `"… and snapshot-DAG test orchestration"` to `"Slipfork CLI & Composition Root: copy-on-write Btrfs dev worlds, worktree forks, and OSPP v1 secrets"`.
2. **Align published `slipfork doctor` transcripts & `next:` prose with `DoctorReport::render_text` (`crates/slipfork/src/doctor.rs:463-506` & `:96-106`):**
   - `docs/getting-started.md:73-85`: put `✓ Ready` on line 1, indent check rows by two spaces, place `unshare_userns` + `netns` before advisory `noatime` + `ingress: :80 active (port-less origins)`, and update line 85 prose from `fix:` to `next:`.
   - `docs/reference/cli.md:24-36`: put `✗ 1 issue` on line 1, indent check rows by two spaces, place `unshare_userns` + `netns` before `noatime: missing` + `ingress: not installed (ephemeral port; run \`sudo slipfork daemon install\`)`, change `fix:` to `  next: sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>`, and update line 36 prose from `fix:` to `next:`.
3. **Keep `${{` expression-delimiter inspection inside canonical `slipfork-snow` (Law 1):**
   - Add `SecretValue::has_unresolved_expression(&self) -> bool` inside `crates/slipfork-snow` (`crates/slipfork-snow/src/provider.rs:149`, using `slipfork-snow`'s expression syntax constants/helpers) and call `value.has_unresolved_expression()` at `crates/slipfork/src/daemon/resolve.rs:111` so zero `${{` delimiter scanning occurs outside `slipfork-snow`.

# Strategist Milestone Triage Ruling (binding for `lead` & Triad reviewers)

- `crates/slipfork-edge/src/activation.rs`: sanctioned by `ARCHITECTURE.md:50` (`systemd` `LISTEN_FDS` adoption via `listenfd` called once from `slipforkd.rs:127`).
- `docs/research/btrfs-crun-benchmarks.md`: sanctioned primary-source owner measurement record (`M3.11`, `ADR-0010` §2).
- `crates/slipfork-dag` (`MapInputHasher`/`SetDigestCache`), `NoProjectOverride`, and `>500` LOC splits: deferred to `0.2.0` (`STP v1` DAG rewrite) to avoid pre-release churn after `M3.30.6`'s verified 97-test live Btrfs VM sign-off.
- `packages/opencode-slipfork/tests/coordinator.critical.test.ts` & `critical_cli.rs`: protected critical suites (`tools/critical_guard.py`) already paired with live `e2e.critical.test.ts` and verified on `165.232.86.174` (`M3.30.6`).

# Acceptance

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` exits `0`.
- `! grep -rn 'parallel test DAG\|snapshot-DAG' crates/ docs/ README.md` exits `0`.
- `make docs-check` and `make check` exit `0`.
- Write NO `RESULT.md`.
