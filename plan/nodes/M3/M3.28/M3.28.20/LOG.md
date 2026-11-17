# Log M3.28.20

## 2026-11-18T00:08 [planner]

created (task) Sandbox fix: RestrictAddressFamilies must allow AF_NETLINK so a world boots through the installed unit (+ ensure_daemon on Btrfs hosts)

## 2026-11-18T00:09 [lead]

status ready -> active

## 2026-11-18T00:09 [lead]

attempt 1/3 started

## 2026-11-18T00:09 [lead]

base 9098b204b338c53a676d8b77052e8434fba45675

## 2026-11-18T00:14 [lead]

edited fields

## 2026-11-18T00:14 [lead]

attempt 2/3 started

## 2026-11-18T00:14 [lead]

attempt 1: invariants 1-2 done and verified (AF_NETLINK rendered + golden + critical_edge pin 41 passed, netlink-in-unit, clippy/fmt/guard/plan OK). Attempt 1 blocked on invariant 3 because the fix needs protected crates/slipfork/tests/critical_slug.rs -> RULING A: paths widened, land the ~15 LOC. The PATH-isolation branch (706-724) asserts an obsolete contract: post-R5 snow resolve is daemon-served with no env in the request and the daemon resolves repo roots with its own fixed PATH, so the test must assert THAT contract (client PATH cannot influence daemon-side resolution), not deletion-by-ignore. The stale Btrfs teardown wait goes because the harness now owns the daemon.

## 2026-11-18T00:18 [lead]

status active -> review

## 2026-11-18T00:18 [lead]

quad round 1 (cruft): FAIL — MAJOR: the family list is spelled 7 times (unit.rs doc x2, source, golden, critical_edge x3) and will drift -> one const used by the renderer and the tests, docs name the const; MAJOR: critical_edge's extra contains('~')/for-loop blocks assert nothing beyond the existing exact-line assert_eq (~12 LOC to delete); MAJOR: ADR-0008:59 still mandates the three-family list (assigned to M3.29.1, which is active). MINORS: cross-file map comment in critical_slug, &str command param built with args.join on every call, start_in's backend branch hidden from the caller.

## 2026-11-18T00:19 [lead]

round 1 (legality): PASS — only AF_NETLINK appended, list still explicit, CapabilityBoundingSet/ProtectSystem/PrivateTmp/ReadWritePaths untouched; the PATH=bin-empty rewrite is genuinely meaningful (empty client PATH still resolves, byte-identical output, canonical slug asserted); ensure_daemon fails closed naming the command with the production is_served probe. MINORS: RESULT must mark the Btrfs-child-path prediction [unverified — M3.28.13 owns it] and file the outliving-harness-daemon teardown as a followup instead of leaving it verbal; ADR-0008 three-family quote is with M3.29.1 (active).

## 2026-11-18T00:19 [lead]

edited fields

## 2026-11-18T00:19 [lead]

attempt 3/4 started

## 2026-11-18T00:19 [lead]

round 1 (ioc): FAIL — MAJOR: std::mem::forget(daemon) is now applied to Backend::Child, so on a Btrfs host ensure_daemon spawns a real slipforkd and never reaps it (one live daemon per scratch; Drop can never run). Fix: return the Daemon to test scope for Child, forget only InProcess. MAJOR: ADR-0008:59 is a second normative source of the sandbox list (assigned to M3.29.1, active). MINORS: the exact line is hardcoded a third time -> derive from one const/golden; unit.rs prose restates the list. UPWARD QUESTION recorded for M3/0.2.0 rather than escalated: if a second engine ships a unit, sandbox requirements must become a capability (UnitSpec/WorldEngine supplies them), not a product literal in the domain renderer.

## 2026-11-18T00:36 [lead]

status review -> done

## 2026-11-18T00:36 [lead]

quad: functional PASS, legality PASS, cruft/ioc quality items closed in attempt 3 (one const SERVICE_RESTRICT_ADDRESS_FAMILIES used by renderer+golden; redundant ~/* asserts deleted; mem::forget only for InProcess so a real Child daemon is reaped by Drop; run_slipfork_with_env fails closed naming its args). Two MINORs logged: harness_daemon hardcodes 'snow resolve' as the failure label for all 7 tests; ADR-0008 three-family quote is M3.29.1's. make check rc=0 (44 suites).
