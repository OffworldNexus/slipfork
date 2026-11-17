# Log M3.28.12

## 2026-11-17T16:33 [planner]

created (task) VM harness: :80 bound via daemon install, tracked support/demo template, login quickstart banner

## 2026-11-17T23:36 [lead]

status ready -> active

## 2026-11-17T23:36 [lead]

attempt 1/3 started

## 2026-11-17T23:36 [lead]

base 19658f88820d5b636850e012fe60b0fd2f96dba2

## 2026-11-17T23:49 [lead]

attempt 1 cap-out after the work; lead-ran verification: pytest tools/tests 138 passed, ruff check+format clean. First live sync revealed a REAL upgrade-path bug: the stale auto-spawned daemon on the VM speaks the pre-lineage protocol, so 'daemon install' fails at its pre-install ingress read with 'missing field lineage — re-run to converge' (re-running cannot converge). Ruling: the harness owns provision-time version skew — stop any running daemon (systemctl units if active + any slipforkd holding <state_dir>/run/daemon.sock) before the product install, bounded wait, fail closed if the socket survives. Pre-1.0 single-version contract is unchanged; the misleading suffix is a .6 nit to log, not a redesign.

## 2026-11-17T23:49 [lead]

attempt 2/3 started

## 2026-11-18T00:08 [lead]

edited fields

## 2026-11-18T00:08 [lead]

edited fields

## 2026-11-18T00:09 [lead]

attempt 2 blocked on two real product bugs, both reproduced outside the harness: (1) slipforkd.service's RestrictAddressFamilies lacks AF_NETLINK so slipfork-world-init's 'ip link set lo up' dies EAFNOSUPPORT and NO world boots through the installed unit; (2) critical_slug 6/7 fail because tests/support/mod.rs::ensure_daemon skips the daemon on Btrfs hosts while snow resolve is daemon-served (pre-existing at 19658f8). Created M3.28.20 for both; M3.28.12 now depends on it. Harness work itself is done and live-proven: provisioning stops the stale daemon by socket holder, install converges, second run rewrites zero units (sha256sum identical), socket active, demo seeded once, banner quoted correctly, pytest 143 + ruff clean, make check 0.

## 2026-11-18T00:18 [lead]

attempt 3/3 started

## 2026-11-18T00:18 [lead]

attempt 2 blocked (real product bugs) -> M3.28.20 fixed AF_NETLINK + ensure_daemon, both verified. attempt 3 = re-run the live proof now that a world can boot through the installed unit.

## 2026-11-18T00:31 [lead]

attempt 3 LIVE PROOF COMPLETE (the milestone acceptance): sync wrote slipforkd.service with AF_NETLINK; systemctl is-active slipforkd.socket => active; slipfork up => [origin] http://demo.localhost; curl -fsS -H 'Host: demo.localhost' http://127.0.0.1/ => 200 (NO PORT) with {"hello":"world","world":"demo"}; ss shows :80 held by systemd(pid1)+slipforkd; second sync => unchanged for all three files with identical sha256; do_vm.py test => exit 0, five suites (engine live 1, slipfork live 1, critical_cli 21, critical_slug 7/7 on the VM, critical_ospp_daemon 4); pytest 143; ruff clean. Only blocker was rs-fmt on crates/slipfork-edge/tests/critical_edge.rs (M3.28.20's in-flight edit) — fixed in .20's followup.
