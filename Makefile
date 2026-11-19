# Slipfork Makefile: one deterministic gate (`make check`) for humans, CI, and agents.

# Compiler and `uv` scratch stays on the XFS disk under `$HOME/.cache`, never the RAM-backed `/tmp`
# tmpfs (`docs/knowledge/LESSONS.md`). `:=` deliberately pins the gate's scratch root, so an
# inherited tmpfs `TMPDIR` cannot put compiler or `uv` scratch back on tmpfs. A caller can still
# override it on the command line (`make TMPDIR=/x …`): command-line variables win over a makefile
# `:=`. The world-booting suites pin their own scratch roots under `$HOME/.cache` and never read
# `TMPDIR`; other suites may use `tempfile`.
TMPDIR := $(HOME)/.cache/slipfork-tmp
export TMPDIR

# Cap `rustc` parallelism so a loaded shared host never forks one compiler per core.
CARGO_BUILD_JOBS ?= 4
export CARGO_BUILD_JOBS

# Suites that boot real Btrfs worlds (and therefore need a Btrfs state root and a serialized run).
# `make rs-test-live` owns them; `make check` never runs them, so this non-Btrfs host stays green.
# Run them on the lab VM: `uv run --no-project tools/do_vm.py test`.
RS_LIVE_SUITES := slipfork-engine:critical_btrfs_live slipfork-engine:critical_engine slipfork-engine:critical_oci_runtime slipfork:critical_btrfs_live slipfork:critical_cli slipfork:critical_l0_wiring slipfork:critical_ospp_daemon slipfork:critical_slug slipfork:critical_vcs_remote

.PHONY: help check fmt rs-fmt rs-lint rs-test rs-test-live ts-test py-lint py-test docs-check guard plan-check bench bench-update docs-serve

$(TMPDIR):
	mkdir -p $(TMPDIR)

help: ## Show available targets
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-22s\033[0m %s\n", $$1, $$2}'

check: plan-check guard py-lint py-test ts-test rs-fmt rs-lint rs-test docs-check ## Run every static, unit, critical, plan, hygiene, TS, Rust, and docs check

fmt: rs-fmt | $(TMPDIR) ## Format Rust and Python tooling
	uvx --offline ruff format tools poc

plan-check: | $(TMPDIR) ## Validate plan/ consistency
	uv run --no-project tools/plan.py validate

guard: | $(TMPDIR) ## Verify repository hygiene and zero [derived]/[guessed] spec markers
	uv run --no-project tools/guard.py

py-lint: | $(TMPDIR) ## Lint and format-check Python harness/PoC scripts
	uvx --offline ruff check tools poc
	uvx --offline ruff format --check tools poc

py-test: | $(TMPDIR) ## Run unit tests for tools/
	uv run --no-project --with pytest pytest -q tools/tests

ts-test: | $(TMPDIR) ## Run critical tests for @slipfork/opencode plugin
	bun test packages/opencode-slipfork/tests

rs-fmt: | $(TMPDIR) ## Check Rust formatting across workspace
	cargo fmt --all -- --check

rs-lint: | $(TMPDIR) ## Run Clippy (-D warnings) and rustdoc (-D warnings) across workspace
	cargo clippy --workspace --all-targets -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# One compile step per target set: `--bins`/`--lib`/`--doc` builds and runs the unit and doc
# tests, `--tests --no-run` links every integration binary exactly once. The per-suite `cargo test
# --test …` lines below then only execute an already-built binary (Cargo's fingerprint is
# up to date), so no target is re-linked and no test shells out to a nested `cargo build`.
rs-test: | $(TMPDIR) ## Run the pure/loopback Rust suites (compile once, no Btrfs needed)
	cargo test --workspace --lib --bins
	cargo test --workspace --tests --no-run
	cargo test --workspace --doc
	uv run --no-project tools/rs_test_list.py --world-booting $(RS_LIVE_SUITES)
	cargo test -p slipfork-dag --test critical_dag
	cargo test -p slipfork-dag --test critical_world_config
	cargo test -p slipfork-edge --test critical_edge
	cargo test -p slipfork-engine --test critical_rootfs_contract
	cargo test -p slipfork-engine --test critical_rootfs_live
	cargo test -p slipfork-ospp --test critical_ospp
	cargo test -p slipfork-snow --test claimed_eval
	cargo test -p slipfork-snow --test critical_snow
	cargo test -p slipfork-vcs --test critical_conformance_matrix
	cargo test -p slipfork-vcs --test critical_vcs
	cargo test -p slipfork-world --test critical_world
	cargo test -p slipfork --test browser
	cargo test -p slipfork --test daemon_unit_install

# The Btrfs world-booting journeys. A direct `cargo test --test …` on a non-Btrfs host fails closed
# through `require_btrfs` (one line naming the missing substrate) — it never skips silently.
rs-test-live: | $(TMPDIR) ## Run the Btrfs world-booting suites (needs Btrfs; lab VM: `tools/do_vm.py test`)
	cargo test --workspace --tests --no-run
	cargo test -p slipfork-engine --test critical_btrfs_live -- --test-threads=1
	cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
	cargo test -p slipfork-engine --test critical_oci_runtime -- --test-threads=1
	cargo test -p slipfork --test critical_btrfs_live -- --test-threads=1
	cargo test -p slipfork --test critical_cli -- --test-threads=1
	cargo test -p slipfork --test critical_l0_wiring -- --test-threads=1
	cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
	cargo test -p slipfork --test critical_slug -- --test-threads=1
	cargo test -p slipfork --test critical_vcs_remote -- --test-threads=1

docs-check: | $(TMPDIR) ## Build public Zensical documentation site in --strict mode and verify CLI honesty
	uvx --offline zensical build --strict
	uv run --no-project tools/check_docs_honesty.py

docs-serve: | $(TMPDIR) ## Preview the Zensical documentation site locally
	uvx --offline zensical serve

bench: | $(TMPDIR) ## Re-measure the PoC benchmark read-only and fail on drift (opt-in, never writes)
	cargo build --release -p slipfork -p slipfork-engine --bins
	uv run --no-project poc/run_benchmark.py --check

bench-update: | $(TMPDIR) ## Re-measure the PoC benchmark and write the report + baseline (review before committing)
	cargo build --release -p slipfork -p slipfork-engine --bins
	uv run --no-project poc/run_benchmark.py --write-report
