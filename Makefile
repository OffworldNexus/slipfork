CARGO ?= cargo
UV ?= uv

# Point this at a directory on a real Btrfs mount (a loop file is fine); never `/tmp`.
SLIPFORK_TEST_ROOT ?=
LIVE_RUNTIME_DIR ?= /dev/shm/slipfork-live-runtime
LIVE_SUITES := \
	slipfork-world/critical_btrfs_live \
	slipfork-image/critical_oci_live \
	slipfork-image/critical_dockerfile_live \
	slipfork-world/critical_pod_runtime \
	slipfork/critical_world_journey

.PHONY: fmt clippy test check guard test-tools live-test

fmt:
	$(CARGO) fmt --all

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	$(CARGO) test --workspace

guard:
	$(UV) run --no-project tools/guard.py

test-tools:
	$(UV) run --no-project -m unittest discover -s tools/tests

live-test:
	@test -n "$(SLIPFORK_TEST_ROOT)" || { \
		echo "live-test: SLIPFORK_TEST_ROOT is unset; next: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test" >&2; exit 2; }
	@test "$$(stat -f -c %t "$(SLIPFORK_TEST_ROOT)" 2>/dev/null)" = 9123683e || { \
		echo "live-test: $(SLIPFORK_TEST_ROOT) is not on Btrfs; next: mount a loop-backed Btrfs file there" >&2; exit 2; }
	@test "$$(stat -f -c %t /dev/shm 2>/dev/null)" = 1021994 || { \
		echo "live-test: /dev/shm is not tmpfs; next: mount a tmpfs to hold XDG_RUNTIME_DIR" >&2; exit 2; }
	@command -v crun >/dev/null 2>&1 || { \
		echo "live-test: crun is missing; next: sudo apt-get install crun" >&2; exit 2; }
	@install -d -m 700 "$(SLIPFORK_TEST_ROOT)" && rm -rf "$(LIVE_RUNTIME_DIR)" && install -d -m 700 "$(LIVE_RUNTIME_DIR)"
	@set -e; for suite in $(LIVE_SUITES); do \
		pkg=$${suite%%/*}; test=$${suite##*/}; \
		printf '[live] %s %s\n' "$$pkg" "$$test"; \
		XDG_RUNTIME_DIR="$(LIVE_RUNTIME_DIR)" SLIPFORK_TEST_ROOT="$(SLIPFORK_TEST_ROOT)" \
			$(CARGO) test -p "$$pkg" --features "$$pkg/live" --test "$$test" -- --test-threads=1; \
	done
