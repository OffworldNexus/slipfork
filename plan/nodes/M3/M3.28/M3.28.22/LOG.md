# Log M3.28.22

## 2026-11-18T04:15 [planner]

created (task) Quiet world entry: the entry argv must start inside the world so the shell's first getcwd succeeds

## 2026-11-18T04:27 [lead]

edited fields

## 2026-11-18T04:27 [lead]

status ready -> ready

## 2026-11-18T04:27 [lead]

attempt 1: root cause solved correctly (nsenter --wd opens the dir in the CALLER's ns then fchdir after setns, so pivot_root leaves a detached cwd; --wdns opens after the switch). entry_prefix is now the single builder, maps the host cwd through the world's bind mounts via world_guest_for_host, falls back to the app-stage root, emits --wdns=, and WorldEntryCommand.cwd is '/'; conformance pins it and the old host-cwd expectation is deleted. critical_engine 16, engine lib 69, critical_osspp_daemon 6, no-getcwd-noise, clippy/fmt/guard/plan/critical_guard green. Parked: invariant 3's orientation line belongs in fn shell (main.rs), which M3.28.11 holds — paths widened, re-dispatch after .11 lands.

## 2026-11-18T04:28 [lead]

edited fields

## 2026-11-18T04:28 [lead]

attempt 1 (final): make check's only failure is the shared conformance pin hitting StubWorldEngine's host cwd in the protected critical_edge (one line). Took option A: path widened for that file (a stub returning the host cwd would let a second adapter regress silently — dropping the pin is the wrong trade). Invariant 3's orientation line stays queued behind M3.28.11, which holds main.rs.

## 2026-11-18T04:34 [lead]

status ready -> active

## 2026-11-18T04:34 [lead]

attempt 1/3 started

## 2026-11-18T04:45 [lead]

status active -> done

## 2026-11-18T04:45 [lead]

attempt 2 complete: stub returns in-world cwd, fn shell prints the one orientation line (stderr, shell only, never --json). critical_engine 16, critical_edge 35, critical_cli 29 (new PTY test), critical_osspp_daemon 6, engine lib 69, no-getcwd-noise, clippy/fmt/guard/plan OK, make check exit 0. Committed with M3.28.11 (main.rs interleave).
