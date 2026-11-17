---
id: M3.28
title: "0.1.0 UX pre-release pass (owner hands-on findings): :80 ingress, bare status, restart/ps/open, fork identity"
kind: epic
status: ready
priority: 1
depends_on: []
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:32"
updated: "2026-11-17T16:32"
---

# Goal

Turn the owner's hands-on `0.1.0` session on the Btrfs DO VM into a shippable UX pass, in `0.1.0`, before the Release PR: a real port-80 ingress installed by one product verb, bare `slipfork` = status, `restart`/`ps`/`open`, a `fork` that needs neither an argument nor the developer's git config, and none of the jargon, silence, or noise the owner hit.

# Context (owner transcript on `165.232.86.174`, requirements)

1. `slipfork up` printed `[origin] http://demo.localhost:40645`; `wget http://demo.localhost` was refused — the daemon auto-spawns `slipforkd --listen 127.0.0.1:0`, and the design's `slipforkd.socket` was never installed.
2. Bare `slipfork` typed 4x wanting status; the full 25-command help wall each time. `slipfork status` was `unrecognized subcommand`.
3. `slipfork restart` was `unrecognized subcommand` (the edit-and-rerun reflex).
4. `ps` / `ps faux` typed at the host shell.
5. `slipfork fork` (no arg) demanded `<FORK>`; `slipfork fork tralala` dumped raw git: `commit-tree … failed (128): Author identity unknown`.
6. `slipfork dag plan` printed nothing at all.
7. `slipfork shell` printed `sh: 0: getcwd() failed: No such file or directory`, then `ps: command not found`.
8. `[resources] demo: prefer_remote -> local (not_offered)` on the happy path.
9. The owner hand-wrote `ssh -L 40645:localhost:40645`; nothing told them the laptop recipe.
10. `getent passwd` archaeology was needed to find the `slipfork` account.
11. `slipfork list` was good but gave no way to open the world.
12. Re-running `slipfork up` in a worktree printed the same URL and no indication whether anything restarted.

Nothing is pushed to GitHub, so no `0.x` compatibility is owed: superseded CLI text and behaviour are deleted in this sweep, never aliased.

# Order of work (DX-first, then code)

- **Phase A (docs first, no code):** `M3.28.1` architect writes `docs/design/dx-0.1.0-ux.md` and locks the contract decisions; `M3.28.2` runs the `dx` + `ai-ergonomics` critic and applies corrections in place.
- **Phase B (`:80` ingress, highest value):** `M3.28.6` `slipfork daemon install`; it depends on the daemon answers in `M3.28.3`/`M3.28.4`; `M3.28.5` (VCS) runs in parallel.
- **Phase C (verbs):** `M3.28.7` status → `M3.28.8` restart → `M3.28.9` ps/open → `M3.28.10` fork. `crates/slipfork/src/cli.rs`, `crates/slipfork/src/main.rs` and `crates/slipfork/tests/critical_cli.rs` are single-writer, so these are chained with `--depends` rather than widened into parallel globs.
- **Phase D (bugs/polish):** `M3.28.11` dag/shell/logs/resources; `M3.28.12` VM harness, tracked demo, login onboarding.
- **Phase E:** `M3.28.13` live re-verification on `165.232.86.174`; `M3.28.14` published docs + `CHANGELOG.md`.

# Acceptance

At the gate (children are the authority; this is the floor):

```console
$ uv run --no-project tools/plan.py validate
plan OK
$ make check
... exit 0
$ make docs-check
... exit 0
$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -o /dev/null -w '%{http_code}\n' -H 'Host: demo.localhost' http://127.0.0.1/
200
```

- Every child is `done`; the gate node is `done` only with the live `M3.28.13` RESULT attached.
- `plan/ROADMAP.md`'s M3 row is **not** edited here (`strategist` owns it).
- Budget: at most one `architect` escalation (`M3.28.1`); zero `expert`.

# Node map (authoritative; supersedes the phase sketch above)

- `M3.28.1` architect: `docs/design/dx-0.1.0-ux.md` + `ARCHITECTURE.md` + `ADR-0011`; locks the five contract decisions.
- `M3.28.2` doc-writer: `dx` + `ai-ergonomics` critic, applied in place.
- `M3.28.3` daemon truth through `DaemonControl` (ingress addrs, origin port, component tree rows) — no CLI edits.
- `M3.28.4` restart-vs-up semantics, port-less origin renderer, engine process-tree accessor (depends `M3.28.3`).
- `M3.28.5` VCS: branch-named fork source, pinned `slipfork` snapshot identity, one-line git errors (parallel).
- `M3.28.6` `slipfork daemon install` (`:80` socket activation) + `doctor` ingress line (`depends` 3, 4).
- Phase C (serialized on `cli.rs`/`main.rs`/`critical_cli.rs`): `M3.28.7` status → `.8` restart → `.9` ps/open → `.10` fork no-arg.
- `M3.28.11` polish (dag plan, quiet shell, log prefixes, `-v` resources); `M3.28.12` VM harness, tracked demo, login banner.
- `M3.28.13` live re-verify on `165.232.86.174`; `M3.28.14` published docs + `CHANGELOG.md`.
