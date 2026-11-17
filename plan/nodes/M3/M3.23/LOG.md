# Log M3.23

## 2026-11-16T18:43 [lead]

created (task) Terminology gate: purge Option A/B and Spike A/B framing from ARCHITECTURE.md, README.md, docs/, crates/, tools/

## 2026-11-16T18:59 [lead]

edited fields

## 2026-11-16T18:59 [lead]

SCOPE ADD (from M3.7 ioc final pass): docs/design/m1-core-and-poc.md still carries stale VolumeCloner/IncusEngine/TestAdapter rows after the Btrfs-only pivot. This gate also deletes/labels stale post-pivot design residue under docs/design/** (keeping the M3.8/M3.9/M3.10 rewrites), with zero banned engine names or dead engine seams left.

## 2026-11-16T19:07 [lead]

ADDITIONAL SCOPE (M3.7 legality final pass): (1) ADR-0010:9 and ADR-0006:13-14 must phrase docs/research/btrfs-crun-benchmarks.md as 'to be recorded by M3.11' (future tense), not 'as measured in the benchmark report' while the file is absent; (2) ARCHITECTURE.md:156  port-doc names crates/slipfork-test without the Phase 2 / M3.2 target-state qualifier. Verify both after M3.11 lands, and keep the repo-wide option/spike grep clean.

## 2026-11-16T19:07 [lead]

ADDITIONAL SCOPE (M3.7 legality final pass): (1) ADR-0010:9 and ADR-0006:13-14 must phrase docs/research/btrfs-crun-benchmarks.md as "to be recorded by M3.11" (future tense), not "as measured in the benchmark report" while the file is absent; (2) ARCHITECTURE.md:156 port-doc comment names crates/slipfork-test without the Phase 2 / M3.2 target-state qualifier. Verify both after M3.11 lands, and keep the repo-wide option/spike grep clean.

## 2026-11-16T19:19 [lead]

FINAL POLISH (from M3.7 cruft sign-off, optional): (1) ADR-0009 Composition Root mermaid redraws the ARCHITECTURE §1 tree -> collapse to a §1 pointer; (2) ADR-0002:19 restates the BtrfsEngine::new -> with_driver constructor-DI sentence also in ADR-0010 §5 -> keep one home; (3) ARCHITECTURE.md:11 test-server token should vanish once M3.22 lands.

## 2026-11-16T20:48 [lead]

FINAL POLISH from M3.7 functional review: ADR-0010 Consequences should state the operational consequence (every non-Btrfs host hard-fails at slipforkd boot and slipfork doctor with EngineError::BtrfsRequired {path, actual_fstype, mount_hint} naming mount -o user_subvol_rm_allowed,noatime; M3.21 deletes all previously shipped adapters/protocol/pools).

## 2026-11-16T21:03 [lead]

SCOPE ADD (M3.11 cruft gate): now that docs/research/btrfs-crun-benchmarks.md exists, ADR-0010 Context §2 and docs/design/world-base-image.md must CITE it instead of restating 19.5 ms / 11-24 ms / 39 ms / 6.3 MiB (report = sole owner of the numbers). Also annotate stale forward references "to be recorded in full by M3.11" (ADR-0010:9, world-base-image.md:124,436) and the M3.7/M3.9 RESULT passages that claim spike-option-b retention.

## 2026-11-16T21:09 [lead]

POLISH (M3.11 legality PASS minors): (1) docs/research/btrfs-crun-benchmarks.md:67 asserts the per-world RSS figure counts init + service containers though the attribution method is recorded Unverified — reword as "presumably"; (2) line 29 should note RSS wording came from ADR-0010 §2, not the owner record.

## 2026-11-16T21:34 [lead]

M3.25 RULING: align ADR-0008 §4 --world wording with ADR-0005 §3 (shared resolve_world_selector accepting <project>, <project>:<fork>, bare <fork>).

## 2026-11-16T21:52 [lead]

OWNER DIRECTIVE (CBOR): purge every NDJSON/JSON-wire mention for STP v1 across ARCHITECTURE.md, README.md, docs/** (incl. docs/decisions/ADR-0006*) and replace with "CBOR (RFC 8949) over the inherited SLIPFORK_PROTOCOL_FD AF_UNIX socketpair". Known offender: docs/design/m1-core-and-poc.md:45 (RunnerProtocol v1 NDJSON) — also delete the stale RunnerProtocol/TestAdapter/CoverageDriver row there. Verify ! grep -riE "ndjson|jsonl|line-delimited" ARCHITECTURE.md README.md docs/ crates/ tools/.

## 2026-11-16T21:53 [lead]

ADD (M3.8 functional PASS): crates/slipfork-engine/src/rootfs/mod.rs:544 BaseImageError::CacheOnTmpfs message still says "XFS, Btrfs, or ext4" — must name Btrfs with the mount options and no alternate filesystem. Also verify no NDJSON/CBOR regressions and purge the stale m1-core-and-poc RunnerProtocol row.

## 2026-11-16T21:55 [lead]

CONFIDENTIALITY (owner Rule #10): tools/guard.py:12 contains a pre-existing sibling-repository name in a provenance aside ("learned from poc-orange"). Reword to "learned from an unrelated external audit" and add to this gate an inverted grep for sibling/client names. Also scrub any host-observation prose in plan/ and node RESULT/LOG files to "unrelated ambient builds on the shared host".

## 2026-11-16T21:56 [lead]

DOC ACCURACY (from M3.8 functional PASS): docs/design/world-base-image.md — (a) line ~300 lists `seaweedfs` as a shipped ServiceSpec id; the shipped id is `s3_buckets` (`seaweedfs` is only BackingServiceKey Display) — fix the id list, keep the BackingServiceKey list; (b) `--appendonly yes` is not shipped (redis spec has no AOF flag) — drop or label M3.13 target state after M3.13 rules; (c) line ~148 says "length prefixes make the encoding injective" but only u32le list counts are prefixed and fields are NUL-delimited — restate accurately; (d) the §8 renumber orphaned `crates/slipfork-engine/src/rootfs/pin.rs:27` doc pointer (§7.1 -> §8.1) — fix the code comment (owned here, crates/** in paths).

## 2026-11-16T22:27 [lead]

DOC POLISH from M3.25 cruft PASS (apply after commit): (1) ADR-0009:16 `slipforkd --crun <path>` is a second supply path for the compile-time-pinned crun — delete or declare once in ARCHITECTURE §1; (2) the crun cache path literal appears in ADR-0009 §1 and ARCHITECTURE §3.3 — keep the path in ADR-0009, only the never-collect rule in §3.3; (3) ARCHITECTURE:423 cites `LiveWorld` (undefined there) — name the real registry/daemon type; (4) ARCHITECTURE:336/341 restate ADR-0010 §2 slots — point at §2; (5) ADR-0009:16 and ADR-0010:48 mutually restate "zero host crun probe in doctor" — keep in ADR-0010 §4; (6) ARCHITECTURE:576-582 restates WorldId grammar owned by ADR-0005 §3 — pointer only; (7) ARCHITECTURE:680 Phase-2 prune comment omits prune_cache.

## 2026-11-16T22:29 [lead]

DOC FIX (M3.25 functional gate, MAJOR): docs/design/target-dx.md §1.6 still says "<fork> equal to the project name is rejected" for <project>:<fork>, contradicting ADR-0005 §3 + ARCHITECTURE §3.4: <project>:<project> is ACCEPTED as UserFork(<project>) (only `slipfork fork <fork>` refuses creating such a fork). Rewrite that rule to "matched by case; <project>:<project> accepted (disambiguated by the colon); <fork> == project rejected only when creating via slipfork fork" so the printed id always round-trips.

## 2026-11-16T23:51 [lead]

OWNER REGROUP RULING (binding): final shipped version is 0.1.0. In this hygiene gate set Cargo.toml workspace version, crates/* Cargo.toml package versions, packages/opencode-slipfork/package.json, CHANGELOG.md (fold 0.2.0 into one 0.1.0 entry describing the shipped Btrfs Worlds surface), zensical.toml and docs version labels to 0.1.0. Also carry the M3.25 record MINORs: WorldHandle Debug, BaseSeedSpec carrying BaseKey, retention/prose dedupe, phantom DropFork request naming drop_fork(WorldSelector), WorldId grammar restatement, undeclared WorldTarget. M3.2/STP v1 is parked to 0.2.0: no STP v1 shipped claims.

## 2026-11-16T23:51 [lead]

edited fields

## 2026-11-17T01:32 [lead]

ADD from M3.16 live proof: docs/design/do-vm-harness.md must record (a) the AppArmor userns profile header requires both bare paths in one rule set /{a,b} (two separate bare-path rules are invalid), (b) 'do_vm.py ssh -- <cmd>' must not require a built CLI and must export CARGO_HOME/RUSTUP_HOME, (c) volume discovery ignores /dev/disk/by-id -partN symlinks, (d) the live Btrfs volume is /dev/sda (no partition) LABEL=slipfork.

## 2026-11-17T05:13 [lead]

ADD from M3.13 gate (docs of record must match 0.1.0 reality): (1) ARCHITECTURE §1/§2 + ADR-0010 §2 currently say WorldSpec.control_dir/state_subdirs are injected and <subvol>/rootfs holds the rootfs tree — Phase 1 derives ctl/<slug>-<seq> in the engine and <subvol>/rootfs is a symlink to cache/rootfs/<digest>/app (snapshots are not self-contained; the cache is a retention root). Relabel those as 0.2.0 (ensure_base) and state the Phase-1 symlink reality. (2) M3.25 target-state items WorldId/resolve_world_selector, WorldSpec.world/control_dir/state_subdirs, BaseKey/BaseSeedSpec/ensure_base, ReadinessProbe, prune_snapshots are 0.2.0. (3) README.md:17 and docs/getting-started.md:26 still advertise XFS/ZFS as supported CoW filesystems and must drop them once BtrfsEngine is the sole engine. (4) crates/slipfork-engine/src/linux_cow.rs doc claims it is the only unsafe module (btrfs.rs also owns unsafe) — fix after M3.21. (5) M3.15 must re-add the daemon unit --only apparmor command to userns_remediation_hint when the flag ships.

## 2026-11-17T11:37 [lead]

ADD (M3.18 cruft gate, all OUT of M3.18 paths): (1) Cargo.toml version 0.2.0 -> 0.1.0 + Cargo.lock; (2) CHANGELOG.md restructure to one 0.1.0 entry; (3) zensical.toml nav label; (4) sibling pages still 0.2.0: docs/snow-modes.md:1,124, docs/reference/manifest.md:1,157-158 (also 'Milestone 3' labels), docs/agents-and-opencode.md:1; (5) packages/opencode-slipfork/package.json version; (6) README/getting-started duplicated Btrfs prerequisite block — keep one and link; (7) crates/slipfork-engine/Cargo.toml description still says 'Incus REST (/1.0) and Linux FICLONE' (M3.21 owns, do it there).

## 2026-11-17T12:22 [lead]

ADD (from M3.18 RESULT followups + lead verification): (1) docs/reference/manifest.md:16,44 and docs/snow-modes.md:17,51 put ${{ components::api.private_port }} inside run_command. run_command is passed verbatim to 'sh -eu -c' (daemon.rs:1590-1600), so it is NOT expression-templated and the published primary example fails at boot with 'Bad substitution'. Fix the docs to name the literal port matching internal_ports:[8000] and add one sentence stating run_command is not templated (the shipped getting-started.md:79 already hardcodes 0.0.0.0:8000 — that is the honest pattern). (2) ARCHITECTURE.md:101 still says project::resolve_world_selector -> WorldId while the shipped code is daemon::resolve_world_selector -> WorldTarget (WorldId is 0.2.0 target state). (3) crates/slipfork/tests/critical_slug.rs git-only branch asserts an inverted contract off-Btrfs (legality MINOR) — either assert the real-child BtrfsRequired refusal or split the harness check out. (4) docs/snow-modes.md:1,124, docs/reference/manifest.md:1,157-158, docs/agents-and-opencode.md:1 still say 0.2.0/'Milestone 3'. (5) docs/design/adr-0008.review-cruft.md and .review-ioc-upward.md are adjudicated critic artifacts still carrying Option A/B framing, and tools/tests/test_tools.py has a 'Use Option A.' fixture string. (6) crates/slipfork-engine/src/rootfs/mod.rs:82 already fixed in M3.21.

## 2026-11-17T12:58 [lead]

edited fields

## 2026-11-17T12:58 [lead]

DEPENDENCY EDIT (lead): removed M3.19. M3.19 is blocked on the owner's H-006 AppArmor direction-challenge (a live-host decision), while this node's work — the 0.1.0 version single-source, CHANGELOG, zensical labels, and the terminology/hygiene greps — is entirely local and must not be held hostage by a remote kernel-security ruling. M3.19's measured numbers land in its own RESULT.md; nothing here depends on them.

## 2026-11-17T13:11 [lead]

ADD (H-006 impact): docs must not claim the 4-line profile alone suffices on stock Ubuntu 24.04. Document BOTH: the AppArmor profile must name every executable that calls unshare(CLONE_NEWUSER) (entrypoint, slipfork-world-init, /usr/bin/unshare, in-world crun) — pending H-006 — and, for a dedicated dev VM, kernel.apparmor_restrict_unprivileged_userns=0 is a verified alternative (persist via /etc/sysctl.d). M3.18's getting-started/cli pages need the correction.

## 2026-11-17T13:42 [lead]

status ready -> active

## 2026-11-17T13:42 [lead]

attempt 1/3 started

## 2026-11-17T13:42 [lead]

base 8417868066bb3af9448dd1aba650b57e25960740

## 2026-11-17T13:56 [lead]

Gate passed: acceptance greps all green, slipfork --version == 0.1.0, cargo build --workspace --locked exit 0, plan validate/guard (505 files)/check_docs_honesty/docs-check/make check exit 0. prune --dry-run kept because a protected critical_cli test pins it, and documented truthfully.

## 2026-11-17T13:56 [lead]

status active -> done
