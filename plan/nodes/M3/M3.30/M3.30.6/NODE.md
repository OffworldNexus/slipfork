---
id: M3.30.6
title: "Live lab VM proof (165.232.86.174): PTY color/spinner E2E, .localhost :80 demo & before/after llvm-cov audit"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.4, M3.30.5]
paths: [docs/knowledge/LESSONS.md, "plan/nodes/M3/M3.30/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T22:19"
updated: "2026-11-19T02:25"
---

# Goal
Prove the swept `0.1.0` on real hardware: full lab E2E green on the droplet, a real PTY owner session with color + spinners, and a measured before/after LOC + coverage delta.

# Context
- Lab droplet: `165.232.86.174`; sync with `tools/do_vm.py sync`, run with `tools/do_vm.py test` (Btrfs + `postgres:18` + OSPP suites).
- Baseline before this epic: `43,271` src LOC, `43.77%` line coverage (`-C instrument-coverage`, `llvm-cov report`) over the core journey.
- Demo world lives at `/home/slipfork/demo`.
- Use `script -qec "<cmd>" /dev/null` to get a real PTY; never trust a piped-only check for color/spinner behavior.

# Deliverables
1. **E2E green:** all consolidated live Btrfs + `postgres:18` + OSPP suites on `165.232.86.174` exit `0` (no skipped/failed).
2. **PTY owner session** against `/home/slipfork/demo`, verifying:
   - real ANSI color + live spinner frames on TTY (`script` capture shows `\x1b[` and progress headers);
   - `NO_COLOR=1` run and `--json` run are escape-free;
   - `slipfork doctor` verdict-first `✓ Ready`;
   - port-less `http://demo.localhost` (no `:80`) returns `200`;
   - bare `slipfork` status dashboard columns aligned;
   - live checkout edit to `app/server.py` + `slipfork restart` serves the updated body immediately;
   - no-arg `fork` with git config unset; `ps`; `logs`; `open --remote`; `drop`; `down`.
3. **Coverage audit:** re-measure `-C instrument-coverage` + `llvm-cov report` over that exact journey; clean up `cov-*` scratch; leave stock `0.1.0` binaries and the `demo` world up on `:80` afterwards.
4. **Numbers recorded** in this node's `LOG.md` (via `plan.py log`) and in `docs/knowledge/LESSONS.md`: before vs after total Rust src LOC and live execution coverage %, plus any lab gotchas worth remembering.

# Constraints
- No `RESULT.md` essays: proof is commands + numbers in `LOG.md` / `LESSONS.md`.
- Do not leave debug instrumentation, `cov-*` dirs, or modified manifests on the VM.
- Keep the demo world serving on `:80` at handoff; kill stray `-pgid` groups and scratch subvolumes.
- Never touch `develop` directly; no force-push, no tags, no `gh pr merge`.

# Acceptance
- `tools/do_vm.py sync && tools/do_vm.py test` → exit `0`.
- PTY transcript shows ANSI + spinner frames, and `NO_COLOR=1` / `--json` transcripts show zero `\x1b[`.
- `curl -s -o /dev/null -w '%{http_code}' http://165.232.86.174/` (or port-forwarded equivalent) → `200`, and the post-restart body matches the edited `app/server.py`.
- `uv run --no-project tools/plan.py log M3.30.6 "coverage: <before> -> <after> LOC, <before%> -> <after%> lines"` recorded.
- `uv run --no-project tools/plan.py validate` exits `0`.
