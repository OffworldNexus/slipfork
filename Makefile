# Slipfork Makefile: one deterministic gate (`make check`) for humans, CI, and agents.

.PHONY: help check fmt lint test rs-fmt rs-lint rs-test ts-test py-lint py-test docs-check guard plan-check support-check support-sync bench bench-update docs-serve

help: ## Show available targets
	@grep -E '^[a-zA-Z0-9_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-22s\033[0m %s\n", $$1, $$2}'

check: plan-check guard py-lint py-test support-check ts-test rs-fmt rs-lint rs-test docs-check ## Run every static, unit, critical, plan, hygiene, support, TS, and docs check

fmt: rs-fmt ## Format Rust and Python tooling
	uvx --offline ruff format tools poc

plan-check: ## Validate plan/ consistency
	uv run --no-project tools/plan.py validate

guard: ## Verify repository hygiene and zero [derived]/[guessed] spec markers
	uv run --no-project tools/guard.py

py-lint: ## Lint and format-check Python harness/PoC scripts
	uvx --offline ruff check tools poc
	uvx --offline ruff format --check tools poc

py-test: ## Run unit tests for tools/
	uv run --no-project --with pytest pytest -q tools/tests

support-check: ## Verify the integration support registry, its evidence pointers, and docs/support-matrix.md (read-only)
	uv run --no-project tools/check_support.py --check

support-sync: ## Demote unsupported tier claims and regenerate docs/support-matrix.md (explicit mutator)
	uv run --no-project tools/check_support.py --sync

ts-test: ## Run critical tests for @slipfork/opencode plugin
	bun test packages/opencode-slipfork/tests

rs-fmt: ## Check Rust formatting across workspace
	cargo fmt --all -- --check

rs-lint: ## Run Clippy (-D warnings) and rustdoc (-D warnings) across workspace
	cargo clippy --workspace --all-targets -- -D warnings
	RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

rs-test: ## Run all Rust unit, integration, critical, and doc tests
	cargo test --workspace --all-targets
	cargo test --workspace --doc

docs-check: ## Build public Zensical documentation site in --strict mode and verify CLI honesty
	uvx --offline zensical build --strict
	uv run --no-project tools/check_docs_honesty.py

docs-serve: ## Preview the Zensical documentation site locally
	uvx --offline zensical serve

bench: ## Re-measure the PoC benchmark read-only and fail on drift (opt-in, never writes)
	cargo build --release -p slipfork
	uv run --no-project poc/run_benchmark.py --check

bench-update: ## Re-measure the PoC benchmark and write the report + baseline (review before committing)
	cargo build --release -p slipfork
	uv run --no-project poc/run_benchmark.py --write-report
