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
