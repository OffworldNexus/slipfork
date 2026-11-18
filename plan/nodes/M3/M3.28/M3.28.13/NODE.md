---
id: M3.28.13
title: "Live re-verification of the 0.1.0 UX pass on the Btrfs DO VM (port-less :80, status, restart, ps, fork)"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.28.11, M3.28.12, M3.28.15, M3.28.21, M3.29.3, M3.29.4]
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:33"
updated: "2026-11-18T00:37"
---

# Goal

Re-run the owner's real session on `165.232.86.174` and prove the twelve complaints are gone, with verbatim output in `RESULT.md`.

# Context

The findings are listed in the milestone Context; `plan/nodes/M3/M3.19/RESULT.md` §12.9 is the old walkthrough this replaces. `M3.28.12` installs the `:80` ingress, the tracked demo, and the banner. Nothing is estimated: a command that cannot run is reported as not run with the reason.

# Design / Invariants

1. Use the existing droplet, volume and identity from `M3.19` §1/§11; do not recreate it. Every claim in `RESULT.md` is a verbatim transcript with its exit code.
2. Evidence per finding: `:80` origin (no port) → 200; bare `slipfork` = status; `slipfork --help` still lists commands and **no longer lists `dag` or `share`** (`slipfork dag plan` and `slipfork share` each exit `2`); `up` prints exactly one `lineage` line with a real digest/cache fact and no `[resources]` jargon; `ps` shows the component + `postgres` rows plus the lineage digests; `restart` applies an edited app response **and re-fires an edited hook** (edit `snow.yml`'s hook command or the app file, run `slipfork restart`, show the old output gone — this is the end-to-end proof `M3.28.17` could only reach at the engine-double boundary); `restart` on a never-booted world exits `1` naming `slipfork up`; `fork` no-arg on a branch with `git config --global user.email` unset; `shell` without `getcwd()` noise; `-v` restores the `[resources]` and per-node lineage detail; `open --remote` recipe pasted from the owner's laptop; login banner instead of `getent passwd`; re-run `up` reports `unchanged`.
3. Regressions: `logs`, `db`, `drop`, `down`, `git fetch slipfork <fork>` and `uv run --no-project tools/do_vm.py test` (five live suites) stay green; teardown leaves no world subvolume outside `/snapshots/`; `slipfork --version` prints `0.1.0`.
4. The final walkthrough is at most four commands for a fresh user.

# Acceptance

```console
$ uv run --no-project tools/do_vm.py status
droplet  slipfork-dev-vm  id=620073485  region=ams3  ip=165.232.86.174
$ uv run --no-project tools/do_vm.py ssh -- systemctl is-active slipforkd.socket
active
$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -o /dev/null -w '%{http_code}\n' -H 'Host: demo.localhost' http://127.0.0.1/
200
$ uv run --no-project tools/do_vm.py ssh -- slipfork --version
slipfork 0.1.0
$ uv run --no-project tools/do_vm.py test
... 5 suites, exit 0
$ test -s plan/nodes/M3/M3.28.13/RESULT.md && grep -c 'EXIT=' plan/nodes/M3/M3.28.13/RESULT.md
12
$ uv run --no-project tools/plan.py validate
plan OK
```

- The `curl` has **no port** in the URL: that is the primary proof.
- `RESULT.md` ends with the droplet state and `tools/do_vm.py down --yes` for the owner; a failed finding returns `STATUS: blocked` instead of a claimed pass.
