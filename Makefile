# Slipfork Makefile: one deterministic gate (`make check`) for humans, CI, and agents.

# Compiler and `uv` scratch stays on the XFS disk under `$HOME/.cache`, never the RAM-backed `/tmp`
# tmpfs (`docs/knowledge/LESSONS.md`). `:=` deliberately pins `TMPDIR`, so an inherited tmpfs
# `TMPDIR` cannot put compiler or `uv` scratch back on tmpfs. The integration suites' own scratch
# roots are hardcoded under `$HOME/.cache` and never read `TMPDIR`.
TMPDIR := $(HOME)/.cache/slipfork-tmp
export TMPDIR

# Cap `rustc` parallelism so a loaded shared host never forks one compiler per core.
CARGO_BUILD_JOBS ?= 4
export CARGO_BUILD_JOBS

.PHONY: help check fmt rs-fmt rs-lint rs-test rs-test-list ts-test py-lint py-test docs-check guard plan-check support-check support-sync bench bench-update docs-serve

$(TMPDIR):
	mkdir -p $(TMPDIR)

help: ## Show available targets
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-22s\033[0m %s\n", $$1, $$2}'

check: plan-check guard py-lint py-test support-check ts-test rs-fmt rs-lint rs-test docs-check ## Run every static, unit, critical, plan, hygiene, support, TS, and docs check

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

support-check: | $(TMPDIR) ## Verify the integration support registry, its evidence pointers, and docs/support-matrix.md (read-only)
	uv run --no-project tools/check_support.py --check

support-sync: | $(TMPDIR) ## Demote unsupported tier claims and regenerate docs/support-matrix.md (explicit mutator)
	uv run --no-project tools/check_support.py --sync

ts-test: | $(TMPDIR) ## Run critical tests for @slipfork/opencode plugin
	bun test packages/opencode-slipfork/tests

rs-fmt: | $(TMPDIR) ## Check Rust formatting across workspace
	cargo fmt --all -- --check

rs-lint: | $(TMPDIR) ## Run Clippy (-D warnings) and rustdoc (-D warnings) across workspace
	cargo clippy --workspace --all-targets -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Integration binaries are listed explicitly (never one `--workspace --all-targets` sweep) so the
# parallel and serialized phases stay auditable. Every `WORLD_BOOTING_TESTS` binary boots a real
# world, runs a real `docker buildx` build, or spawns a real `slipforkd`, and runs alone, one case
# at a time. `rs-test-list` mechanically enforces the list and every serialization pair; the
# invariants and the explicit human-maintained classification rule live in the
# `tools/rs_test_list.py` module docstring.
WORLD_BOOTING_TESTS := \
	slipfork-engine:critical_rootfs_live \
	slipfork-engine:critical_engine \
	slipfork-engine:critical_oci_runtime \
	slipfork:critical_slug \
	slipfork:critical_vcs_remote \
	slipfork:critical_cli \
	slipfork:critical_l0_wiring \
	slipfork:critical_ospp_daemon

rs-test-list: | $(TMPDIR) ## Guard the rs-test binary list, its package pairs, and world-booting serialization
	@uv run --no-project tools/rs_test_list.py --world-booting $(WORLD_BOOTING_TESTS)

rs-test: rs-test-list | $(TMPDIR) ## Run all Rust unit, integration, critical, and doc tests
	cargo test --workspace --lib --bins
	cargo test --workspace --doc
	cargo test -p slipfork-dag --test critical_dag
	cargo test -p slipfork-dag --test critical_support
	cargo test -p slipfork-dag --test critical_world_config
	cargo test -p slipfork-edge --test critical_edge
	cargo test -p slipfork-engine --test critical_rootfs_contract
	cargo test -p slipfork-ospp --test critical_ospp
	cargo test -p slipfork-snow --test critical_snow
	cargo test -p slipfork-vcs --test critical_vcs
	cargo test -p slipfork-world --test critical_world
	cargo test -p slipfork-engine --test critical_rootfs_live -- --test-threads=1
	cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
	cargo test -p slipfork-engine --test critical_oci_runtime -- --test-threads=1
	cargo test -p slipfork --test critical_slug -- --test-threads=1
	cargo test -p slipfork --test critical_vcs_remote -- --test-threads=1
	cargo test -p slipfork --test critical_cli -- --test-threads=1
	cargo test -p slipfork --test critical_l0_wiring -- --test-threads=1
	cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1

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
