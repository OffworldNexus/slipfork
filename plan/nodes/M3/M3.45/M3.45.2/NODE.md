---
id: M3.45.2
title: "Live Btrfs VM gate: .deb, 7 critical suites, <1.0s fork, S3 matrix, frozen-pane PTY"
kind: task
status: active
owner: implementer
priority: 3
depends_on: [M3.45.1]
paths: ["crates/**", "scripts/**", "tools/**", ".github/**", TESTING_PLAN.md]
attempts: 3
max_attempts: 3
escalation: expert
created: "2026-11-21T22:40"
updated: "2026-11-22T00:35"
---

# Goal
Prove the shipped 0.1.0 product on a real Btrfs host: install the fresh `.deb` on `165.232.86.174`, arm both daemons, and run all seven live kernel-critical suites with isolated roots, including a hard `< 1.0s` end-to-end fork, the native S3 verb/policy matrix, and a real PTY capture of the `indicatif` frozen pane.

# Context
- VM: `165.232.86.174` (DigitalOcean, Btrfs volume). Driver: `tools/do_vm.py` — `status` (do_vm.py:2026), `sync` (do_vm.py:2105: replaces the tree, builds binaries, installs into `/usr/local/bin`, runs `slipfork daemon install`), `test` (do_vm.py:2196: builds+installs then streams the live suites), `ssh` (do_vm.py:2281) for anything not streamed. Suites get isolated roots from `SLIPFORK_TEST_ROOT` + tmpfs `XDG_RUNTIME_DIR` (do_vm.py:2158-2159, `SUITE_RUNTIME_DIR` do_vm.py:120) — heavy state never on disk `/tmp`.
- Required suite set = all of Makefile:8-15: `critical_btrfs_live`, `critical_oci_live`, `critical_dockerfile_live`, `critical_pod_runtime`, `critical_world_journey`, `critical_status_live`, `critical_fork_live`. `do_vm.py test` help still says "five" — run the remainder explicitly over `ssh` (`cargo test -p <crate> --features live --test <suite>`) and state in your report which ran streamed vs manually.
- Package: build with `scripts/build-deb`, copy the `.deb` to the VM, `dpkg -i` it, then `sudo slipfork daemon install --ingress` (system `:80` socket) **and** `slipfork daemon install` (per-user unit).
- `< 1.0s` proof: wall time from process spawn to `http://<name>--<world>.localhost` answering `200` with Postgres + Redis + S3 + the multi-service app, measured with a real `std::time::Instant` in `crates/slipfork/tests/critical_fork_live.rs` plus a shell `time` cross-check. Assert zero image rebuilds and zero redundant `pre-deploy` re-runs (boot plan served from the image cache) and quote the measured milliseconds.
- S3 matrix: native Btrfs S3 owner modules `crates/slipfork-world/src/resources/s3/policy.rs`, `object_ops.rs`, `listing.rs`; `.localhost` routing in `crates/slipfork-ingress/` (journey shape: `tests/route_table_journey.rs`). Required: `PUT`/`GET`/`HEAD`/`DELETE`/`ListObjectsV2` on `policy private` (`403` anonymous `GET`, `200` SigV4 `GET`) and `policy public-read` + `cdn static-cdn` (`200` anonymous `GET` on `http://static-cdn.<world>.localhost`, `403` anonymous `PUT`).
- Frozen pane proof: `indicatif::MultiProgress` pane at `crates/slipfork/src/boot/pane.rs:12-22`, byte ownership at `crates/slipfork/src/boot/render.rs:3`, driver split at `crates/slipfork/src/boot.rs:3`. Capture through a real PTY (Braille spinner + 16-cell bar + rolling tail visible), then re-run under `NO_COLOR=1` and `TERM=dumb` and assert `0` occurrences of `\x1b[` in the captured bytes (harness shape: `crates/slipfork/tests/ansi_matrix.rs`, `status_surface.rs`).
- Honesty: zero `RESULT.md` and zero `[derived]`/`[guessed]` numbers — paste measured fork time, HTTP status lines and escaped-byte count in your final message. A failing suite is reported failing: never rerun to green, never skip a suite silently.

# Acceptance
- `uv run --no-project tools/do_vm.py status` shows the tagged droplet/volume up before and after.
- `dpkg -i` of the fresh `.deb` succeeds; `systemctl status slipfork.socket` plus the per-user unit are active; `slipfork doctor` exits 0 on the VM.
- All seven suites pass on the VM; your report lists each suite, whether streamed or manual, and its test count.
- `crates/slipfork/tests/critical_fork_live.rs` asserts `elapsed < Duration::from_millis(1000)`, the measured value is quoted, and two consecutive runs both pass.
- The S3 matrix is asserted in tests, not shell-only: 5 verbs × both policies with the exact status codes above, plus the ingress `static-cdn` anonymous `200` `GET` / `403` `PUT` cases.
- PTY test asserts Braille + 16-cell bar + rolling tail are present, and that `NO_COLOR=1`/`TERM=dumb` captures contain zero `\x1b[`.
- `cargo clippy --workspace --all-targets -- -D warnings` exits 0 after any test edits; `uv run --no-project tools/critical_guard.py` exits 0 (no critical test weakened).
