# Log M3.15

## 2026-11-16T16:58 [planner]

created (task) slipforkd BtrfsEngine wiring, slipfork doctor Btrfs probe, daemon unit --only apparmor userns profile

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T20:48 [lead]

REQUIREMENT (M3.7 functional review): the same 0.3.0 sweep must delete the stale doctor help text and JSON fields describing the old engine (crates/slipfork/src/cli.rs:40, main.rs:173-180) so published docs and CLI agree with ADR-0010.

## 2026-11-16T20:59 [lead]

SPEC OWNERSHIP (from M3.9 legality review): doctor output fields need a normative owner — ADR-0010 §4 only defines BtrfsProbeReport (fstype/magic/probe evidence). M3.15 must own: (a) the fstype i64 -> filesystem name rendering table (btrfs shown as "btrfs"), (b) netns probe evidence (loopback bring-up), (c) crun probe evidence (version/exec), (d) unshare_userns probe evidence including the AppArmor EPERM hint, and (e) the exact JSON keys for all of them, so published docs can quote real output.

## 2026-11-16T21:34 [lead]

M3.25 RULING APPLIED to NODE.md: doctor reports BtrfsProbeReport fields via doctor::run(&dyn SubvolumeDriver, &dyn HostProbe, ...) + HostProbe netns/unshare_userns/world_rootfs_crun (no host-binary crun probe); --world accepts <project>, <project>:<fork>, and bare <fork> via project::resolve_world_selector (ADR-0005 §3); list/forward --json world uses <project>[:<fork>].

## 2026-11-16T21:43 [lead]

CORRECTION (M3.9 attempt 2): doctor performs NO crun probe and emits NO crun field (ADR-0010 §4). world_rootfs_crun removed from the doctor contract; static crun is provisioned by crates/slipfork::runtime::runtime_assets into <state_dir>/cache/runtime/ and staged into WorldRootfs.crun_bin, never a host PATH lookup.

## 2026-11-16T21:45 [lead]

M3.25 RULING: wire HostCheckOutcome (two-variant; NetnsCheck::Skipped synthesized only in doctor::run), DoctorReport declared once in crates/slipfork::doctor, resolve_world_selector(selector: &str, repo: &RepoRoot) -> Result<WorldTarget, WorldError> with raw world + cwd on the wire; no crun field in doctor.

## 2026-11-16T22:50 [lead]

M3.25 final ruling to consume: WorldId newtype (Display/FromStr/parse_selector, TestSlot never on wire), DropForkRequest { cwd, world: Option<WorldId> }, PruneReport declared once, WorldSpec.world, BaseKey fields promoted, DoctorReport.fixes order btrfs->unshare_userns->netns, EngineError world:String via WorldId::to_string().

## 2026-11-17T01:40 [lead]

ADD (cross-node ruling from M3.16 ioc gate): your 'daemon unit --only apparmor' output must byte-match tools/do_vm.py's render_apparmor_body() text (the harness must install the profile before any binary exists, so the copy is legitimate but must be guarded). Add a test that runs the built slipfork binary and compares the rendered bytes to the harness renderer (tools/tests/test_do_vm.py is authorized for this one file). Paths widened: tools/tests/test_do_vm.py.

## 2026-11-17T01:40 [lead]

edited fields

## 2026-11-17T05:32 [lead]

status ready -> active

## 2026-11-17T05:32 [lead]

attempt 1/3 started

## 2026-11-17T05:32 [lead]

base 3bc93e3ef6be13ce9d5c2e8e2326565960483686

## 2026-11-17T06:02 [lead]

edited fields

## 2026-11-17T06:02 [lead]

Paths widened by lead for two reds the node exposed: (1) Makefile must classify + serialize slipfork-engine:critical_btrfs_live and slipfork:critical_btrfs_live in rs-test / WORLD_BOOTING_TESTS (M3.1 guard fails closed otherwise); (2) the opencode live e2e must be dual-mode like the Rust live suites — on a non-Btrfs host assert the BtrfsRequired fail-closed contract with a diagnostic instead of failing the whole suite, so plain make check stays green on a dev host (M3 exit criterion).

## 2026-11-17T06:18 [lead]

status active -> review

## 2026-11-17T06:18 [lead]

worker: slipforkd wired to BtrfsEngine, doctor seam + JSON shape, --only apparmor, in-process test harness, dual-mode live suites, Makefile classification, TS e2e dual-mode. make check exit 0 (16/16 ts, both live suites 1 pass). Quad gate starting.

## 2026-11-17T06:40 [lead]

edited fields

## 2026-11-17T06:40 [lead]

LEAD RULING (ioc blind spot, 0.1.0 subset): implement the minimal round-trip selector — accept bare <fork>, <project>:<fork>, and canonical <project> in select/drop without the WorldId newtype; list output must be paste-able back into --world. M3.23 relabels ADR-0005/ARCHITECTURE to say the newtype lands in 0.2.0. Also widened for tools/do_vm.py (VM SUITES must run the positive CLI/slug/ospp suites).

## 2026-11-17T12:17 [lead]

Quad Review Gate re-run (reviewer+legality+ioc+cruft) on the 053596e working tree, scoped diff /tmp/opencode/lead/m3.15.diff

## 2026-11-17T12:18 [lead]

Quad gate 1/2 results: legality PASS (3 MINOR: critical_slug git-only branch asserts inverted contract off-Btrfs — loud+VM-listed; critical_ospp_daemon early-return skip documented+VM-gated; test_do_vm SLIPFORK_BIN override accepts any file). ioc MAJOR adjudicated by lead as DEFERRED-to-0.2.0, not a defect: wait_for_postgres_ready's Instant/sleep bounded poll matches the established production idiom (engine service.rs:229, btrfs_engine.rs:730/1701, main.rs:1209) and ReadinessProbe::Exec is explicitly 0.2.0 target state (M3.23 LOG 2026-11-17T05:13); ioc MINORs: composer lives in daemon.rs (0.2.0 layering), harness forward_port None (RESULT wording), test-only BUILD_ONCE, critical_slug hidden daemon side effect.

## 2026-11-17T12:34 [lead]

make check gate 1: RED only at rs-lint — rustdoc private_intra_doc_links on doctor.rs:8 [filesystem_name] after the cruft fix made it private. Fixing (plain code span) + RESULT §6 addendum for the shipped private resolve_world_selector. Everything before rs-lint was green (plan/guard/ruff/pytest 122/ts-test 16/fmt/clippy).

## 2026-11-17T12:36 [lead]

Gate passed: make check gate 1 green except rs-lint (private_intra_doc_links); fixed → RUSTDOCFLAGS=-D warnings cargo doc -p slipfork exit 0, make rs-lint exit 0. Quad: reviewer PASS after fix, legality PASS, ioc MAJORs adjudicated/deferred to 0.2.0, cruft MINORs applied. critical_guard --base 053596e --node M3.15 OK.

## 2026-11-17T12:36 [lead]

status review -> done
