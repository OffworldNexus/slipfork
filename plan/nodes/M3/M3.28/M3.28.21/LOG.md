# Log M3.28.21

## 2026-11-18T00:37 [planner]

created (task) daemon install owns the socket handover for any holder (version-agnostic, ownership-proven), so the harness keeps no daemon lifecycle

## 2026-11-18T00:37 [lead]

status ready -> active

## 2026-11-18T00:37 [lead]

attempt 1/3 started

## 2026-11-18T00:37 [lead]

base 9e392f6eae75153ed25264ac2648b7c3686a06ab

## 2026-11-18T00:41 [lead]

edited fields

## 2026-11-18T00:41 [lead]

attempt 2/3 started

## 2026-11-18T00:41 [lead]

attempt 1 blocked correctly: SO_PEERCRED needs an unsafe getsockopt, but every candidate host file is #![forbid(unsafe_code)] and std peer_cred() is unstable. RULING option 1: widen paths to crates/slipfork-engine/src/linux_cow.rs (the existing audited unsafe home, already depends on libc) for a 12-line peer_pid mirroring signal_process; option 3 (/proc/net/unix inode scan) rejected — it silently re-implements the ss discovery this node deletes and is racy.

## 2026-11-18T00:57 [lead]

status active -> review

## 2026-11-18T01:05 [lead]

attempt 3/3 started

## 2026-11-18T01:05 [lead]

gate FAIL (legality x2 BLOCKER): (1) /proc/<pid>/exe NotFound => Ok(None) => release returns Ok without re-probing is_served(), so a live holder becomes 'no holder' and install proceeds with a held socket — must re-probe and fail closed naming pid; (2) SIGKILL escalates the STALE pid captured before the 15s grace (pid reuse => root kills an innocent process) — proof must hold at signal time (pidfd, or re-prove the same pid before each signal). MAJORs: peer_pid lets pid 0 through (kill(0) = process group!) => reject <= 0; the ownership proof has no adapter-level test; one probe (is_served vs socket_holder) can disagree; doc claims purity it lost. RULING on the ioc blind spot: no new trait name for 0.1.0 — fold served()/shutdown() through the injected port so there is ONE probe, and drop the engine re-exports so this node does not touch lib.rs (it collides with M3.28.7's pub mod status).

## 2026-11-18T01:20 [lead]

round 1 (functional): PASS — shutdown answerer released with zero signals; foreign proven holder Terminate->full grace->Kill; unowned holder refused with path/pid/sudo kill and zero signals (also proven on a real listener); no holder => no writes/signals. MINORs accepted: NODE acceptance amended (Layout guarantees an absolute runtime dir, so 'empty path' is not a defended case; the guard + its test stay removed) and the RESULT wording for [ok] unchanged / zero effects corrected by the worker. Remaining attempt-3 work: the two legality BLOCKERs (NotFound != released; pidfd-pinned signals) plus pid-0 rejection, one probe, real-proof test, lib.rs untouched.

## 2026-11-18T01:26 [lead]

committed together with M3.28.7 for the same reason (interleaved hunks in main.rs/critical_cli.rs).

## 2026-11-18T01:26 [lead]

status review -> done
