---
id: M3.70.2
title: "Live VM proof: 3 CLI suites + .deb + prod-check -F -dev + demo/feat-auth dev"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.70.1]
paths: ["plan/nodes/M3/M3.70/**"]
attempts: 2
max_attempts: 5
escalation: expert
created: "2026-11-23T13:30"
updated: "2026-11-23T15:23"
---

# Goal

Prove `M3.69` + `M3.70.1` live on `165.232.86.174` (Btrfs dev VM, access via `uv run --no-project tools/do_vm.py ssh`), as unprivileged user `slipfork`, with the shipped glibc<=2.34 `.deb` installed.

# Steps

1. `uv run --no-project tools/do_vm.py sync` (installs current debug binaries + units; the demo template seed is once-only and will NOT update `/home/slipfork/demo`).
2. Run the three live CLI suites as `slipfork` with `SLIPFORK_TEST_ROOT=/var/lib/slipfork/t` and a real `XDG_RUNTIME_DIR`: `critical_world_journey`, `critical_fork_live`, `critical_status_live` (record pass counts).
3. Build locally `make packages-bundle` + `scripts/build-deb.sh --version 0.1.0 --bundle target/package-bundle --max-glibc 2_34 --out dist/packages`, copy the `.deb` to the VM, `dpkg -i` it, and confirm `/usr/bin/slipfork --version` plus a `features` help line.
4. Resync `/home/slipfork/demo` to the new layout (`api/` + `front/` + `Forkfile`), removing the old root `app/`, `pyproject.toml`, `uv.lock`, `Dockerfile`; update `.forks/feat-auth` and `.forks/tralala` the same way. Restart `demo` and `feat-auth` in dev mode; leave `tralala` stopped.
5. Create `slipfork fork prod-check -F -dev` from `/home/slipfork/demo`, assert: no `mailpit`/`mail` route, `front` serves `node build` from the Dockerfile `CMD`, `api` serves `uvicorn` from its Dockerfile `CMD`, `curl -sI http://prod-check--demo.localhost/` and `curl -s http://prod-check--demo.localhost/api/status` return 200, then `slipfork drop prod-check`.
6. Final state: `demo` + `feat-auth` running in dev (`200` on `http://demo.localhost/` and `http://feat-auth--demo.localhost/`), `tralala` stopped.

# Acceptance

- Three live suites green as `slipfork` (paste exact test counts).
- `curl` evidence for prod-check `200` on `/` and `/api/status` with `-F -dev`, and `mailpit` absent from `slipfork ps`/`features` output.
- `demo`/`feat-auth` healthy in dev mode, `tralala` stopped at the end; all evidence in your report (no RESULT.md).
