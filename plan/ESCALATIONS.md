# Escalations to the expensive model

Every call to an expensive-tier agent (`strategist`, `architect`, `expert`) is recorded here before spawning it, via `uv run --no-project tools/plan.py --agent <caller> escalate <node> --to <agent> --reason "..."`.

| time | node | to | by | reason |
|---|---|---|---|---|
| 2026-11-14T23:16 | M1.6 | architect | conductor | Integrate owner M1 design decisions (.localhost daemon, weaved IPC, git/jj remote, language-agnostic RunnerProtocol) |
| 2026-11-15T00:26 | M1.6 | architect | lead | Quad gate attempt 2 SPEC_BLIND_SPOT: TunnelLease ownership (WorldRegistry/daemon vs CLI), slipforkd release_tunnel IPC port, and WorldTarget/ProjectSlug single rule-table + project identity type ownership |
| 2026-11-15T00:42 | M1.6 | expert | lead | Quad gate attempt 3: legality BLOCKERs (plugin --snow-file regression + unactionable error), ioc MAJOR (slipfork.toml dual ownership [test].after_suite vs DagConfig), jj/git root guessing, missing crates/slipfork e2e slug tests; attempts exhausted |
| 2026-11-15T01:03 | M1.6 | architect | lead | Final gate legality BLOCKER: git submodule/separate-git-dir root misidentification (fix spec) + jj canonical root unverifiable without owner-approved .jj/repo contract; rule fail-closed+defer vs documented pointer read |
| 2026-11-15T02:01 | M1 | strategist | conductor | milestone gate |
| 2026-11-15T02:28 | M1 | strategist | conductor | milestone gate re-run after M1.7 repaired make bench |
| 2026-11-15T10:38 | M1.8 | architect | conductor | owner design corrections at release gate: single canonical origin per world, support tiers, typed coverage replacing after_suite shell recipes |
| 2026-11-15T10:39 | M1.8 | architect | lead | Owner review-gate corrections: single-origin invariant, support tiers, typed coverage + OSPP precedence clarification |
| 2026-11-15T11:23 | M1 | strategist | conductor | final milestone gate for 0.1.0 after owning M1.8 design corrections and M1.9 code alignment |
| 2026-11-15T16:24 | M2 | strategist | conductor | Amend ROADMAP M2/M3 for owner directives (WebSocket OSPP + tpmd auth + DB dump + ResourceStrategy, hook debugging, systemd user slipforkd, 31-byte service truncation, db/redis/forward CLI) and decompose M2 |
| 2026-11-15T16:46 | M2.1 | architect | lead | M2.1: author ADR-0008 and align ADR/ARCHITECTURE/target-DX (core DI tree lock) |
| 2026-11-15T20:11 | M2.24 | architect | conductor | core architecture crossroads: L0 world base image strategy (Dockerfile-derived rootfs vs curated base), image contract, cache/invalidation, DI port for two engines |
| 2026-11-15T20:23 | M2.24 | architect | lead | M2.24: design ADR-0009 L0 world base image from the project's own Dockerfile + 3-layer composition + World Image Contract |
| 2026-11-16T01:55 | M2 | strategist | conductor | milestone gate for 0.2.0 (36 leaves done; M2.21/M2.22 blocked on H-004/H-006 as designed) |
| 2026-11-16T06:19 | M2 | strategist | conductor | M2 gate re-run after M2.30/M2.31/M2.32 remediation |
| 2026-11-16T17:28 | M3.7 | architect | unknown | Option A Btrfs-only design of record: ADR-0010, ARCHITECTURE.md DI tree, ADR-0001..0009 wipe |
| 2026-11-16T17:57 | M3.7 | architect | unknown | Quad review attempt 1: SPEC_BLIND_SPOT SubvolumeDriver test-driver visibility (cfg(test) unreachable from crates/slipfork tests) + 3 MAJOR IoC (FileSystemKind capability enum, duplicated statfs probe ownership, undefined <run_dir>) + legality/cruft MAJORs (dangling research citation, netns.sock path drift, duplicated ioctl contract, VISION M2 vs M4.1) |
| 2026-11-16T18:39 | M3.7 | architect | unknown | attempt 3: final wording/contract fixes (Phase-2 sent-as-shipped fact x3, ADR-0008 second readiness mechanism, ADR-0009 image-contract readiness source, Layout::resolve owner, BtrfsProbeReport through the driver seam, language tokens in core row, WorldEngine::name) |
| 2026-11-16T18:59 | M3.7 | architect | unknown | attempt 4 (budget raised): four one-line wording/contract clarifications after all three lenses PASS (VM-harness unshare present-tense claim, slipfork-test target-state label, doctor probe seam, BtrfsProbeReport field spec) |
| 2026-11-16T21:00 | M3.25 | architect | unknown | SPEC_BLIND_SPOT from M3.9 IoC gate: doctor netns/crun/unshare_userns probes have no DI port (M3.15 requires them) and world identity has two spellings (display <project>:<fork> vs --world <fork>) |
| 2026-11-17T03:21 | M3.20 | architect | lead | SPEC_BLIND_SPOT (legality BLOCKER): ARCHITECTURE §3.3 declares WorldHandle opaque with no constructor yet trait WorldEngine is public and requires out-of-crate impls to return one; the only out-of-crate impl is a test double, so M3.20 added the provided Self:Sized mint_world_handle seam. Rule the sanctioned contract + amend §3.3 (trait listing + handle doc), then the code doc must match. |
| 2026-11-17T12:45 | M3.19 | architect | lead | H-006: AppArmor attachment set for world launchers; ADR-0010 §7 mechanism is false (children execve out of the profile), so the profile must name every executable that calls unshare(CLONE_NEWUSER). Architect to adjudicate + record the corrected §7 and the exact attachment set before any code change. |
| 2026-11-17T16:34 | M3.28.1 | architect | lead | DX of record for the 0.1.0 UX pass (epic-sactioned single architect escalation) |
| 2026-11-18T00:09 | M3.29.1 | architect | lead | OSPP v1 spec ripple across ARCHITECTURE/ADRs/DX (epic-sanctioned) |
| 2026-11-18T14:06 | M3.28.24 | expert | lead | repeated pre-flight/rollback correctness findings in reload_world; attempt cap reached |
| 2026-11-19T06:47 | M3 | strategist | conductor | 0.1.0 milestone gate |
| 2026-11-19T12:42 | M3 | architect | conductor | Lock Snowfile DSL grammar, per-service crun pod model, and clean-sheet CLI UX spec |
| 2026-11-19T15:00 | M3.32.2.2.1.3.2.1 | architect | lead | pod flip writes resolved remote secrets into OCI config.json under state_dir; crun copies it into its --root, so critical_ospp_daemon no-plaintext-on-disk invariant fails. Need DI decision: tmpfs runtime-root for bundles vs env transport channel. |
| 2026-11-19T23:06 | M3.32.6 | expert | lead | critical_btrfs_live: namespaces.json publishes /proc/1/ns/ipc (host PID 1); nsenter Permission denied + crun kill leak on teardown |
