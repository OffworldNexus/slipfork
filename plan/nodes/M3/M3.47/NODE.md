---
id: M3.47
title: "Fix CI live-test HOME/SLIPFORK_TEST_ROOT Btrfs wiring & container UID 1001 cleanup in build-apk.sh / scripts/*.sh"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths: [Makefile, "scripts/**", ".github/workflows/**", "crates/slipfork/tests/critical_*.rs"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-22T10:36"
updated: "2026-11-22T10:36"
---

# Goal

Close the final two GitHub Actions runner issues observed in `CI` run `45007559861` and `Packages` run `45007559842` so all PR checks pass 100% green:

1. **`CI` (`live` job -> `Makefile` `live-test` & `crates/slipfork/tests/critical_*`):**
   - `critical_btrfs_live`, `critical_oci_runtime`, `critical_dockerfile_runtime`, and `critical_pod_runtime` ALL passed on the GHA loop-backed Btrfs runner!
   - `critical_world_journey.rs` failed closed at line 132 with:
     `fail closed: '/home/runner/.cache/slipfork/tests' is not on Btrfs.`
     because `Makefile` `live-test` left `HOME=/home/runner` (ext4) while `tools/do_vm.py` sets `HOME=<btrfs>/home` + `TMPDIR=<btrfs>/tmp` (preserving `CARGO_HOME` and `RUSTUP_HOME`), AND `critical_world_journey.rs` / `critical_fork_live.rs` / `critical_status_live.rs` should also honor `SLIPFORK_TEST_ROOT`!
   - Fix `Makefile` `live-test` to create `"$(SLIPFORK_TEST_ROOT)/home"` and `"$(SLIPFORK_TEST_ROOT)/tmp"` and pass `HOME="$(SLIPFORK_TEST_ROOT)/home" TMPDIR="$(SLIPFORK_TEST_ROOT)/tmp" CARGO_HOME="$${CARGO_HOME:-$$HOME/.cargo}" RUSTUP_HOME="$${RUSTUP_HOME:-$$HOME/.rustup}" XDG_RUNTIME_DIR="$(LIVE_RUNTIME_DIR)" SLIPFORK_TEST_ROOT="$(SLIPFORK_TEST_ROOT)"` (matching `tools/do_vm.py`), AND ensure `critical_world_journey.rs`, `critical_fork_live.rs`, and `critical_status_live.rs` resolve `SLIPFORK_TEST_ROOT` cleanly too!
   - Also make sure `git` inside those live tests works cleanly when `HOME` has no `.gitconfig` (all 3 live suites already set `GIT_AUTHOR_*`/`GIT_COMMITTER_*` or check every `git commit` call in `crates/slipfork/tests/critical_*.rs` so none fails `Author identity unknown` on a fresh `HOME`!).

2. **`Packages` (`build-apk.sh` & all `docker run` packaging scripts on UID `1001` GHA runners):**
   - Both `amd64` and `arm64` built all 6 package formats successfully (`apk: dist/packages/slipfork-0.1.0-r0-x86_64.apk`), and failed ONLY in `scripts/build-apk.sh`'s `EXIT` cleanup `rm -rf /var/tmp/slipfork-apk.*` (`Permission denied`) because inside `docker run alpine`, `builder` is UID `1000` while on GitHub Actions `runner` is UID `1001`!
   - Audit **every** `docker run` across `scripts/build-apk.sh`, `scripts/build-rpm.sh`, `scripts/build-bundle.sh`, and `Makefile`: ensure the container `chown -R "$(id -u):$(id -g)"`s any bind-mounted host output/work directory (`trap 'chown -R HOST_UID:HOST_GID /work ...' EXIT` inside the container, or clean `/work` inside the container as root before exit) so host UID `1001` (or any non-1000 UID) NEVER hits `Permission denied` on cleanup or artifact upload!
   - Also verify `uv run --no-project scripts/publish_packages.py push --format deb --file ... --component main --dry-run` exits `0` when `CLOUDSMITH_API_KEY` and `FORGEJO_TOKEN` are empty (`""`), since GitHub Actions passes empty strings for unset secrets on `pull_request`!

# Acceptance

- `make check`, `make packages`, and `make packages-test` exit `0`.
- `env CLOUDSMITH_API_KEY="" FORGEJO_TOKEN="" uv run --no-project scripts/publish_packages.py push --format deb --file dist/packages/slipfork_0.1.0_amd64.deb --component main --dry-run` exits `0`.
- Simulated non-1000 UID / container ownership check on `build-apk.sh` / `build-rpm.sh` / `build-bundle.sh` leaves zero root- or UID-1000-owned files when invoked by another UID.
