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
	slipfork/critical_world_journey \
	slipfork/critical_status_live \
	slipfork/critical_fork_live

.PHONY: fmt clippy test check guard test-tools live-test lsp-assets js-test typecheck packages-check

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
	bash editors/zed/check.sh
	$(MAKE) packages-check

# The plugin's tests spawn the real binary from this workspace, never a stand-in.
js-test:
	$(CARGO) build -p slipfork
	cd packages/opencode-slipfork && test -d node_modules || bun install --silent
	cd packages/opencode-slipfork && SLIPFORK_BIN=$(CURDIR)/target/debug/slipfork bun test

typecheck:
	cd packages/opencode-slipfork && test -d node_modules || bun install --silent
	cd packages/opencode-slipfork && npx tsc --noEmit

packages-check: typecheck js-test

guard:
	$(UV) run --no-project tools/guard.py

test-tools:
	$(UV) run --no-project -m unittest discover -s tools/tests

# Builds the Zed extension's release asset for one target. The name (and the
# archive's top-level directory) must match `asset_for` in editors/zed/src/lib.rs.
LSP_TARGET ?=
lsp-assets:
	@set -eu; \
	target="$(LSP_TARGET)"; \
	if [ -z "$$target" ]; then target=$$(rustc -vV | sed -n 's/^host: //p'); fi; \
	case "$$target" in \
	  x86_64-*-linux-*) platform=linux; arch=x86_64 ;; \
	  aarch64-*-linux-*) platform=linux; arch=aarch64 ;; \
	  x86_64-apple-darwin) platform=darwin; arch=x86_64 ;; \
	  aarch64-apple-darwin) platform=darwin; arch=aarch64 ;; \
	  x86_64-*-windows-*) platform=windows; arch=x86_64 ;; \
	  aarch64-*-windows-*) platform=windows; arch=aarch64 ;; \
	  *) echo "lsp-assets: no release asset for $$target; next: pass TARGET=<linux|darwin|windows triple> on x86_64 or aarch64" >&2; exit 2 ;; \
	esac; \
	name="slipfork-lsp-$$platform-$$arch"; \
	$(CARGO) build -p slipfork-lsp --bin slipfork-lsp --release --target "$$target"; \
	stage="dist/$$name"; \
	rm -rf "$$stage"; \
	mkdir -p "$$stage"; \
	if [ "$$platform" = windows ]; then \
	  cp "target/$$target/release/slipfork-lsp.exe" "$$stage/slipfork-lsp.exe"; \
	else \
	  cp "target/$$target/release/slipfork-lsp" "$$stage/slipfork-lsp"; \
	fi; \
	if [ -f LICENSE ]; then \
	  cp LICENSE "$$stage/LICENSE"; \
	else \
	  printf 'Slipfork is distributed under the terms published with the source at\nhttps://github.com/OffworldNexus/slipfork\n' > "$$stage/LICENSE"; \
	fi; \
	if [ "$$platform" = windows ]; then \
	  command -v zip >/dev/null 2>&1 || { echo "lsp-assets: zip is missing; next: install zip to build the Windows archive" >&2; exit 2; }; \
	  ( cd dist && zip -qr "$$name.zip" "$$name" ); \
	  echo "dist/$$name.zip"; \
	else \
	  tar -czf "dist/$$name.tar.gz" -C dist "$$name"; \
	  echo "dist/$$name.tar.gz"; \
	fi

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
