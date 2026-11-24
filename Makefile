CARGO ?= cargo
UV ?= uv
UVX ?= uvx

# Point this at a directory on a real Btrfs mount (a loop file is fine); never `/tmp`.
SLIPFORK_TEST_ROOT ?=
LIVE_RUNTIME_DIR ?= /dev/shm/sf-live-runtime
LIVE_SUITES := \
	slipfork-world/critical_btrfs_live \
	slipfork-image/critical_oci_live \
	slipfork-image/critical_dockerfile_live \
	slipfork-world/critical_pod_runtime \
	slipfork-world/pod_control_lab \
	slipfork/critical_world_journey \
	slipfork/critical_status_live \
	slipfork/critical_fork_live

.PHONY: fmt clippy test check coverage guard test-tools live-test lsp-assets js-test typecheck packages-check docs-check docs-serve install uninstall

fmt:
	$(CARGO) fmt --all

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace

# Local line coverage only: this builds and runs the suites that need no `live`
# feature. Rows whose proving story lives in §2/§5/§6 (Btrfs, crun, OSPP live)
# are proven by `make live-test` on a real Btrfs mount; `make coverage` implies
# no `make coverage-live`. Profiles and reports land in `target/cov` (the tool
# sets CARGO_TARGET_DIR); the first instrumented build takes ~20 minutes once.
COVERAGE_TESTS ?= --workspace --tests
COVERAGE_FILES ?= $(shell find crates -path '*/src/*' -name '*.rs' | sort)

coverage:
	$(UV) run --no-project tools/coverage_report.py --tests "$(COVERAGE_TESTS)" --files "$(COVERAGE_FILES)" --live-suites "$(LIVE_SUITES)"

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

# A strict build treats any warning as a failure; the honesty gate then proves
# every published `slipfork <verb>` exists in `slipfork --help`.
docs-check:
	$(UVX) --offline zensical build --strict
	$(UV) run --no-project tools/check_docs_honesty.py

docs-serve:
	$(UVX) --offline zensical serve

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
	@case "$(SLIPFORK_TEST_ROOT)" in /|/home|/home/*|/root|/root/*|/etc|/etc/*|/usr|/usr/*|"$$HOME") \
		echo "live-test: refusing $(SLIPFORK_TEST_ROOT) as SLIPFORK_TEST_ROOT; next: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test" >&2; exit 2 ;; esac
	@test "$$(stat -f -c %t "$(SLIPFORK_TEST_ROOT)" 2>/dev/null)" = 9123683e || { \
		echo "live-test: $(SLIPFORK_TEST_ROOT) is not on Btrfs" >&2; \
		echo "next: sudo mkdir -p $(SLIPFORK_TEST_ROOT) && sudo truncate -s 12G $(SLIPFORK_TEST_ROOT).img && sudo mkfs.btrfs -f $(SLIPFORK_TEST_ROOT).img && sudo mount -o loop,user_subvol_rm_allowed,noatime $(SLIPFORK_TEST_ROOT).img $(SLIPFORK_TEST_ROOT)" >&2; exit 2; }
	@test "$$(stat -f -c %t /dev/shm 2>/dev/null)" = 1021994 || { \
		echo "live-test: /dev/shm is not tmpfs; next: mount a tmpfs to hold XDG_RUNTIME_DIR" >&2; exit 2; }
	@command -v crun >/dev/null 2>&1 || { \
		echo "live-test: crun is missing; next: sudo apt-get install crun" >&2; exit 2; }
	@rm -rf "$(SLIPFORK_TEST_ROOT)/home" "$(SLIPFORK_TEST_ROOT)/tmp" && install -d -m 700 "$(SLIPFORK_TEST_ROOT)" "$(SLIPFORK_TEST_ROOT)/home" "$(SLIPFORK_TEST_ROOT)/tmp" || { echo "live-test: cannot create $(SLIPFORK_TEST_ROOT)/home; next: sudo chown \"$$(id -u):$$(id -g)\" $(SLIPFORK_TEST_ROOT)" >&2; exit 2; } && \
		chmod 700 "$(SLIPFORK_TEST_ROOT)/home" "$(SLIPFORK_TEST_ROOT)/tmp" && \
		rm -rf "$(LIVE_RUNTIME_DIR)" && install -d -m 700 "$(LIVE_RUNTIME_DIR)"
	@set -e; live_cargo_home="$${CARGO_HOME:-$$HOME/.cargo}"; \
		live_rustup_home="$${RUSTUP_HOME:-$$HOME/.rustup}"; \
		for suite in $(LIVE_SUITES); do \
		pkg=$${suite%%/*}; test=$${suite##*/}; \
		printf '[live] %s %s\n' "$$pkg" "$$test"; \
		HOME="$(SLIPFORK_TEST_ROOT)/home" TMPDIR="$(SLIPFORK_TEST_ROOT)/tmp" \
			CARGO_HOME="$$live_cargo_home" RUSTUP_HOME="$$live_rustup_home" \
			XDG_RUNTIME_DIR="$(LIVE_RUNTIME_DIR)" SLIPFORK_TEST_ROOT="$(SLIPFORK_TEST_ROOT)" \
			$(CARGO) test -p "$$pkg" --features "$$pkg/live" --test "$$test" -- --test-threads=1; \
	done

# ---------------------------------------------------------------------------
# Development installs: a branch build into your own prefix, never /usr.
# ---------------------------------------------------------------------------
PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
TARGET_DIR ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)
DEV_BINS := slipfork slipfork-lsp slipfork-world-init

# The package and `install.sh --user` own the well-known bins: refuse /usr, /bin and /sbin,
# and never clobber a binary this Makefile did not put there (receipt: .slipfork-dev-install).
SYSTEM_BINS := /usr|/usr/|/usr/*|/bin|/bin/|/bin/*|/sbin|/sbin/|/sbin/*
RECEIPT := .slipfork-dev-install

install:
	@set -eu; \
	case "$(BINDIR)" in \
	  "") echo "install: refusing an empty BINDIR; next: install the package, or set PREFIX=/some/user/dir make install" >&2; exit 2 ;; \
	  $(SYSTEM_BINS)) echo "install: refusing $(BINDIR); next: install the package, or set PREFIX=/some/user/dir make install" >&2; exit 2 ;; esac; \
	if [ ! -f "$(BINDIR)/$(RECEIPT)" ]; then \
	  for bin in $(DEV_BINS); do \
	    if [ -e "$(BINDIR)/$$bin" ] || [ -L "$(BINDIR)/$$bin" ]; then \
	      echo "install: $(BINDIR)/$$bin exists and was not installed by make; next: remove it or set PREFIX=$$HOME/.local/opt/slipfork make install" >&2; exit 2; \
	    fi; \
	  done; \
	fi; \
	$(CARGO) build --release --target-dir "$(TARGET_DIR)" -p slipfork --bin slipfork; \
	$(CARGO) build --release --target-dir "$(TARGET_DIR)" -p slipfork-lsp --bin slipfork-lsp; \
	$(CARGO) build --release --target-dir "$(TARGET_DIR)" -p slipfork-world --bin slipfork-world-init; \
	install -d -m 0755 "$(BINDIR)"; \
	for bin in $(DEV_BINS); do install -m 0755 "$(TARGET_DIR)/release/$$bin" "$(BINDIR)/$$bin"; done; \
	( cd "$(BINDIR)" && sha256sum $(DEV_BINS) > "$(RECEIPT)" ); \
	echo "installed $(DEV_BINS) -> $(BINDIR); next: put $(BINDIR) before /usr/bin in PATH, then run hash -r"; \
	dev_exe="$(BINDIR)/slipfork"; \
	unit_state=""; \
	if command -v systemctl >/dev/null 2>&1; then \
	  unit_state=$$(systemctl --user show -p LoadState --value slipfork 2>/dev/null || true); \
	fi; \
	if [ "$$unit_state" = loaded ] && systemctl --user is-active --quiet slipfork 2>/dev/null; then \
	  running=$$(systemctl --user show -p ExecStart --value slipfork 2>/dev/null | grep -o 'path=[^ ;]*' | head -n 1 | cut -d= -f2); \
	  if [ -z "$$running" ]; then \
	    pid=$$(systemctl --user show -p MainPID --value slipfork 2>/dev/null || true); \
	    case "$$pid" in ''|0) pid="" ;; esac; \
	    [ -z "$$pid" ] || running=$$(readlink -f "/proc/$$pid/exe" 2>/dev/null || true); \
	  fi; \
	  same=""; \
	  if [ -n "$$running" ]; then \
	    [ "$$(readlink -f "$$running" 2>/dev/null || echo "$$running")" != "$$(readlink -f "$$dev_exe" 2>/dev/null || echo "$$dev_exe")" ] || same=1; \
	  fi; \
	  if [ -z "$$running" ]; then \
	    echo "slipfork: packaged slipfork.service is still active; next: systemctl --user stop slipfork" >&2; \
	  elif [ -z "$$same" ]; then \
	    if systemctl --user stop slipfork >/dev/null 2>&1; then \
	      echo "✓ stopped the packaged slipfork.service — the dev CLI will start the dev daemon"; \
	    else \
	      echo "slipfork: packaged slipfork.service is still active; next: systemctl --user stop slipfork" >&2; \
	    fi; \
	  fi; \
	fi

# Removes exactly the three binaries it installed, proven by the receipt; never systemd units,
# the store, or a binary from the package / `install.sh`.
uninstall:
	@set -eu; \
	case "$(BINDIR)" in \
	  "") echo "uninstall: refusing an empty BINDIR; next: remove the package, or set PREFIX=/some/user/dir make uninstall" >&2; exit 2 ;; \
	  $(SYSTEM_BINS)) echo "uninstall: refusing $(BINDIR); next: remove the package, or set PREFIX=/some/user/dir make uninstall" >&2; exit 2 ;; esac; \
	receipt="$(BINDIR)/$(RECEIPT)"; \
	[ -f "$$receipt" ] || { echo "uninstall: no make install receipt in $(BINDIR); next: remove those files yourself, or use the installer's --uninstall" >&2; exit 2; }; \
	( cd "$(BINDIR)" && sha256sum -c --quiet "$(RECEIPT)" >/dev/null 2>&1 ) || { echo "uninstall: $(BINDIR) no longer holds the make-installed binaries; next: remove them yourself, or use the installer's --uninstall" >&2; exit 2; }; \
	for bin in $(DEV_BINS); do rm -f "$(BINDIR)/$$bin"; done; \
	rm -f "$$receipt"; \
	echo "removed $(DEV_BINS) from $(BINDIR)"; \
	unit_state=""; \
	if command -v systemctl >/dev/null 2>&1; then \
	  unit_state=$$(systemctl --user show -p LoadState --value slipfork 2>/dev/null || true); \
	fi; \
	if [ "$$unit_state" = loaded ] && ! systemctl --user is-active --quiet slipfork 2>/dev/null \
	  && systemctl --user start slipfork >/dev/null 2>&1; then \
	  echo "✓ restarted the packaged slipfork.service"; \
	fi

# ---------------------------------------------------------------------------
# Distribution artifacts (specs/06-distribution-and-docs.md §1-§8).
# ---------------------------------------------------------------------------
VERSION ?= 0.1.0
# `CHANNEL=nightly` needs the run, UTC date and commit that produced the build;
# `scripts/release_stamp.py` turns them into the per-ecosystem stamps below.
CHANNEL ?= stable
STAMP_DATE ?= $(shell date -u +%Y%m%d)
STAMP_RUN ?= 0
STAMP_SHA ?= $(shell git rev-parse --short=7 HEAD 2>/dev/null || true)
PACKAGES_OUT ?= dist/packages
PACKAGES_BUNDLE ?= target/package-bundle
PACKAGES_MUSL_BUNDLE ?= target/package-bundle-musl
MUSL_TARGET ?= $(shell uname -m)-unknown-linux-musl
MUSL_IMAGE ?= rust:alpine

# One gnu bundle, compiled on glibc 2.34, so Rocky 9 (glibc 2.34) installs it.
# A host newer than the floor builds the gnu bundle in `rockylinux:9`; the same
# `--max-glibc` ceiling is then what the builders stamp into Depends/Requires.
# `HOST_GLIBC` is empty on a non-glibc host (musl, BSD), which means a host build
# and no ceiling: set `GNU_CONTAINER=rockylinux:9` explicitly there.
HOST_GLIBC := $(shell ldd --version 2>/dev/null | sed -n '1s/^ldd .*[^0-9]\([0-9][0-9]*\.[0-9][0-9]*\)$$/\1/p')
GNU_CONTAINER ?= $(shell v=$(HOST_GLIBC); [ -n "$$v" ] && [ "$$v" != 2.34 ] && [ "$$(printf '2.34\n%s\n' "$$v" | sort -V | tail -n1)" = "$$v" ] && echo rockylinux:9)
GNU_MAX_GLIBC ?= 2_34
GNU_TARGET_DIR ?= target/package-gnu-target
GNU_CARGO_HOME ?= target/package-cargo-home
GNU_BUNDLE := --max-glibc $(GNU_MAX_GLIBC) \
	$(if $(strip $(GNU_CONTAINER)),--container $(GNU_CONTAINER) --target-dir $(GNU_TARGET_DIR) --cargo-home $(GNU_CARGO_HOME))

.PHONY: packages packages-test packages-bundle packages-probe

# Every artifact must be non-empty and parse as its own format. A truncated
# download or a half-written package fails here, not at install time.
packages-probe:
	@set -e; \
	[ -s $(PACKAGES_OUT)/SHA256SUMS ] || { echo "error: $(PACKAGES_OUT)/SHA256SUMS is missing; next: run make packages" >&2; exit 1; }; \
	for f in $(PACKAGES_OUT)/*.deb; do [ -s "$$f" ] || { echo "error: missing or empty $$f; next: run make packages" >&2; exit 1; }; dpkg-deb -I "$$f" >/dev/null || { echo "error: $$f is not a valid .deb; next: re-run make packages" >&2; exit 1; }; done; \
	for f in $(PACKAGES_OUT)/*.rpm; do [ -s "$$f" ] || { echo "error: missing or empty $$f; next: run make packages" >&2; exit 1; }; if command -v rpm >/dev/null 2>&1; then rpm -qpl "$$f" >/dev/null || { echo "error: $$f is not a valid .rpm" >&2; exit 1; }; else docker run --rm -v "$(PWD)/$(PACKAGES_OUT)":/pkg:ro rockylinux:9 rpm -qpl "/pkg/$$(basename "$$f")" >/dev/null || { echo "error: $$f is not a valid .rpm" >&2; exit 1; }; fi; done; \
	for f in $(PACKAGES_OUT)/*.pkg.tar.zst; do [ -s "$$f" ] || { echo "error: missing or empty $$f; next: run make packages" >&2; exit 1; }; tar --zstd -tf "$$f" >/dev/null || { echo "error: $$f is not a valid pacman package" >&2; exit 1; }; done; \
	for f in $(PACKAGES_OUT)/*.apk $(PACKAGES_OUT)/*.tar.gz; do [ -s "$$f" ] || { echo "error: missing or empty $$f; next: run make packages" >&2; exit 1; }; tar -tzf "$$f" >/dev/null || { echo "error: $$f is not a valid gzip archive" >&2; exit 1; }; done; \
	printf "artifacts in %s:\n" "$(PACKAGES_OUT)"; ls -1 $(PACKAGES_OUT)

packages-bundle:
	scripts/build-bundle.sh --out $(PACKAGES_BUNDLE) $(GNU_BUNDLE)

# Real host-arch artifacts from one measured bundle. rpm and apk use a container
# only because this host has neither rpmbuild nor abuild; the musl bundle is
# compiled in Alpine because the host has no musl toolchain. The gnu bundle is
# compiled on glibc 2.34 (rockylinux:9 when the host is newer), and the builders
# stamp that ceiling into Depends/Requires.
packages:
	@set -eu; \
	stamps="$$($(UV) run --no-project scripts/release_stamp.py env --base '$(VERSION)' --channel '$(CHANNEL)' --date '$(STAMP_DATE)' --run '$(STAMP_RUN)' --sha '$(STAMP_SHA)')" || exit 1; \
	eval "$$stamps"; \
	rm -rf $(PACKAGES_OUT) && mkdir -p $(PACKAGES_OUT); \
	scripts/build-bundle.sh --out $(PACKAGES_BUNDLE) $(GNU_BUNDLE); \
	scripts/build-deb.sh --version "$$DEB_VERSION" --bundle $(PACKAGES_BUNDLE) --max-glibc $(GNU_MAX_GLIBC) --out $(PACKAGES_OUT); \
	scripts/build-rpm.sh --version "$$RPM_VERSION" --release "$$RPM_RELEASE" --bundle $(PACKAGES_BUNDLE) --max-glibc $(GNU_MAX_GLIBC) --out $(PACKAGES_OUT); \
	scripts/build-arch.sh --version "$$ARCH_VERSION" --release "$$ARCH_PKGREL" --bundle $(PACKAGES_BUNDLE) --max-glibc $(GNU_MAX_GLIBC) --out $(PACKAGES_OUT); \
	scripts/build-standalone.sh --bundle $(PACKAGES_BUNDLE) --out $(PACKAGES_OUT); \
	scripts/build-bundle.sh --out $(PACKAGES_MUSL_BUNDLE) --target $(MUSL_TARGET) --max-glibc auto --container $(MUSL_IMAGE); \
	scripts/build-apk.sh --version "$$APK_VERSION" --pkgrel "$$APK_PKGREL" --bundle $(PACKAGES_MUSL_BUNDLE) --out $(PACKAGES_OUT); \
	scripts/build-standalone.sh --bundle $(PACKAGES_MUSL_BUNDLE) --out $(PACKAGES_OUT); \
	( cd $(PACKAGES_OUT) && sha256sum *.deb *.rpm *.pkg.tar.zst *.apk *.tar.gz *.tar.gz.sha256 > SHA256SUMS ); \
	$(MAKE) --no-print-directory packages-probe

# Fast, no-container checks: layout, portable ELF, shell syntax, the release
# stamp grammar, and a real file:// install into a throwaway prefix.
packages-test: packages-bundle
	scripts/check-bundle-layout.sh $(PACKAGES_BUNDLE)
	bash -n scripts/*.sh docs/install.sh
	dash -n docs/install.sh
	$(UV) run --no-project scripts/release_stamp.py self-test
	scripts/check-install.sh --bundle $(PACKAGES_BUNDLE)
	$(MAKE) --no-print-directory packages-probe
