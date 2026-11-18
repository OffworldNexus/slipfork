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

## 2026-11-18T00:36 [lead]

status active -> review

## 2026-11-18T00:37 [lead]

edited fields

## 2026-11-18T00:37 [lead]

gate (ioc): SPEC_BLIND_SPOT — render_daemon_handover is a SECOND daemon lifecycle that bypasses DaemonInstaller::release_running_daemon, because the product release fails closed when it cannot parse a foreign-version daemon's status(). RULING (a): the product owns the handover version-agnostically (new M3.28.21); the harness then calls only 'daemon install' and keeps no process-table logic. Also MAJORs for .12: read state/runtime/socket paths and unit names from the product instead of copying Layout defaults; derive PRODUCT_BINARIES from cargo metadata; quote bin-dir operands.

## 2026-11-18T00:37 [lead]

edited fields

## 2026-11-18T00:38 [lead]

status review -> active

## 2026-11-18T00:38 [lead]

edited fields

## 2026-11-18T00:38 [lead]

attempt 4/4 started

## 2026-11-18T00:38 [lead]

gate (legality): FAIL — BLOCKER: support/demo/README.md and the /etc/profile.d login banner advertise 'slipfork open' and 'slipfork restart', which do not exist yet (Phase C), i.e. vaporware seeded onto every VM; README also carries fabricated transcripts. RULING: (1) ship only verbs that exist in the CURRENT binary; (2) make the harness assert every verb the banner/README names appears in 'slipfork --help' so it cannot rot; (3) extend tools/check_docs_honesty.py to scan support/demo/README.md (paths widened). MINORS: move the local !! secret into modes.local; match the ss local-address field exactly () instead of substring; shlex.quote config paths in remote commands; guard the banner on ~/demo existing; disclose that sync/test SIGTERM the socket holder.

## 2026-11-18T00:44 [lead]

gate (functional): FAIL on two blockers that are not this node's fault: (1) the tree does not compile because M3.28.21 is mid-edit (daemon_unit_install.rs:318 calls a not-yet-written release_daemon) — .12 re-runs after .21 lands; (2) critical_guard was invoked with base 19658f8 while .7's critical_cli.rs (+242) is uncommitted, so the base must be this node's own start and only this node's protected edits judged. MAJORs folded: delete render_daemon_handover once .21 lands; NODE invariant 4 fixed to the shipped-verbs rule (banner prints 'slipfork list', not the unshipped 'open'); acceptance now uses '--with pytest' and 'uvx --offline ruff' because bare 'uv run pytest'/'uv run ruff' cannot spawn. RESULT transcripts to be regenerated after .21.

## 2026-11-18T00:57 [lead]

edited fields

## 2026-11-18T00:57 [lead]

attempt 5/5 started

## 2026-11-18T00:57 [lead]

attempt 4: vaporware eliminated (banner+README name only real verbs; db demo not psql; DEMO_SECRET in modes.local), check_docs_honesty now scans support/demo/README.md with a negative test, harness handover workaround deleted (render_daemon_handover + 6 tests + socket_wait knobs), build inputs from cargo metadata with profile driving path, paths shlex-quoted, banner guarded, description/help honest. LIVE: second sync unchanged + identical sha256, socket active, up => [origin] http://demo.localhost, curl 200 no port re-verified, do_vm.py test exit 0 (1/1/21/7/4), pytest 141, ruff clean, guard OK 590. Blocked only on M3.28.21's rustdoc link (public fn doc -> private HANDOVER_GRACE) which makes make check stop at rs-lint; .21 fix in flight, then .12 re-runs make check.
