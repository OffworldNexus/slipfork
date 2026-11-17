# Log M3.13

## 2026-11-16T16:58 [planner]

created (task) BtrfsEngine: sole production WorldEngine (unified per-world subvolume, RO snapshots, writable clones, --ipc); delete IncusEngine, ReflinkNetnsEngine and FICLONE

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T20:45 [lead]

REQUIREMENT (M3.1 legality gate): the engine stop path must be bounded-but-abort-aware — do not carry a fixed grace (the current 10s WORLD_STOP_GRACE red-lines world-booting suites under host load ~40). SIGTERM, then poll for exit within a bound, escalate to SIGKILL only after the bound, and report the elapsed time so suites can assert real teardown instead of waiting a fixed window.

## 2026-11-16T21:56 [lead]

REQUIREMENTS (from M3.8 functional PASS, real findings): (1) seed-subvolume crash safety — a crash after seal but before destroying worlds/seed-<base_key_sha12> leaves a seed subvol whose seed.complete was written pre-seal, so the "no seed.complete => destroy" rule never fires and Phase-2 GC (snapshots/artifacts/cache only) leaks it forever: under the same flock, sweep worlds/seed-<base_key_sha12> and its ctl/ dir before the early Ok(()) base-hit return; (2) redis argv — the shipped redis spec is `docker-entrypoint.sh redis-server --dir /data` with NO AOF flag; if the design requires `--appendonly yes`, add it to this node and to the redis ServiceSpec wiring, otherwise drop the claim; (3) --host-caches belongs to `slipfork up`/`fork` (and M3.2 test), not to a slipforkd flag (DaemonCli has none; slipforkd attaches caches unconditionally).

## 2026-11-16T22:50 [lead]

M3.25 final ruling to consume: WorldId newtype (Display/FromStr/parse_selector, TestSlot never on wire), DropForkRequest { cwd, world: Option<WorldId> }, PruneReport declared once, WorldSpec.world, BaseKey fields promoted, DoctorReport.fixes order btrfs->unshare_userns->netns, EngineError world:String via WorldId::to_string().

## 2026-11-17T04:44 [lead]

status ready -> active

## 2026-11-17T04:44 [lead]

attempt 1/3 started

## 2026-11-17T04:44 [lead]

base a06100dd9acfc7c05fe91227f01bec04b6a0eba6

## 2026-11-17T05:02 [lead]

status active -> review

## 2026-11-17T05:02 [lead]

worker done: btrfs_engine.rs 2556 lines, 12 unit tests, clippy/fmt/check/docs green; rs-test red only on legacy critical_engine (10s grace + /tmp XFS assumption). Quad gate starting. Rootfs is a symlink reference to the content-addressed cache (deviation from ADR-0010 wording) -> reviewers must judge; M3.23 to reconcile.

## 2026-11-17T05:32 [lead]

FINAL: functional PASS, legality PASS, ioc PASS (blind spot deferred by ruling + M3.23 relabel), cruft PASS — minors logged: create_snapshot should reject a live world's dir (point at freeze_world); SnapshotExists message; treat driver ENOENT as already-destroyed on retry; pre-existing linux_cow fd-count race. Committing.

## 2026-11-17T05:32 [lead]

status review -> done
