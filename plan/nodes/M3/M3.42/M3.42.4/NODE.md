---
id: M3.42.4
title: Live Btrfs proof on 165.232.86.174 without disturbing the owner demo
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.42.3]
paths: ["support/**", "crates/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T17:30"
updated: "2026-11-21T19:40"
---

# Goal

Prove the shipped `.deb` and the seven live Btrfs suites on the Ubuntu 24.04 Btrfs droplet
`165.232.86.174`, and verify the running owner demo stays healthy — without ever restarting
`slipfork.socket` or rebuilding the remote tree.

# Context

- **Forbidden on this droplet:** `tools/do_vm.py sync` and `tools/do_vm.py test`. Both rebuild the
  remote tree, stop `slipfork.socket`, and kill running worlds; `expert` is running a live owner demo
  in `/home/slipfork/demo`. Allowed: `tools/do_vm.py ssh -- <cmd>`, `tools/do_vm.py status`, or
  `ssh root@165.232.86.174`.
- Transfer the locally built artifact by streaming stdin:
  `ssh root@165.232.86.174 'cat > /var/tmp/slipfork.deb' < dist/packages/slipfork_0.1.0_amd64.deb`
  (`dist/packages/` already holds `.deb`/`.rpm`/`.pkg.tar.zst`/`.apk` + `SHA256SUMS`).
- `LIVE_SUITES` (`Makefile` lines 8-15) = `slipfork-world/critical_btrfs_live`,
  `slipfork-image/critical_oci_live`, `slipfork-image/critical_dockerfile_live`,
  `slipfork-world/critical_pod_runtime`, `slipfork/critical_world_journey`,
  `slipfork/critical_status_live`, `slipfork/critical_fork_live` — run each as
  `cargo test -p <pkg> --features <pkg>/live --test <suite> -- --test-threads=1`.
- `live-test` (`Makefile` line 102) fails closed unless `SLIPFORK_TEST_ROOT` is on Btrfs
  (magic `9123683e`) and `/dev/shm` is tmpfs; use `SLIPFORK_TEST_ROOT=/var/tmp/slipfork-live` and
  `XDG_RUNTIME_DIR=/dev/shm/slipfork-live-runtime` so demo worlds, sockets and state stay untouched.
- Demo endpoints to verify on `:80`: `http://demo.localhost`, `http://feat-auth--demo.localhost`,
  `http://mail.demo.localhost`, `http://static_cdn.demo.localhost`; PTY matrix under
  `COLORTERM=truecolor`, `NO_COLOR=1`, `TERM=dumb`; fork timings from `slipfork fork`.
- Refresh only the `support/demo` and `feat-auth` forks if they are actually broken; never destroy
  other demo state.

# Acceptance

- `dpkg -i /var/tmp/slipfork.deb` exits 0 on the droplet and `slipfork --version` prints `0.1.0`.
- `slipfork daemon install` and `sudo slipfork daemon install --ingress` exit 0; `systemctl is-active slipforkd.socket` prints `active`.
- All seven `LIVE_SUITES` pass on the droplet with the isolated `SLIPFORK_TEST_ROOT`/`XDG_RUNTIME_DIR`.
- `curl -sS -o /dev/null -w '%{http_code}'` prints `200` for all four demo URLs.
- The PTY matrix shows zero escape bytes under `NO_COLOR=1`/`TERM=dumb` and colour under `COLORTERM=truecolor`.
- `slipfork fork` on the demo world reports `< 50ms`, and `/home/slipfork/demo` plus every pre-existing world are still listed by `slipfork status` afterwards.
