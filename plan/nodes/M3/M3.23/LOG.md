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
