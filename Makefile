CARGO ?= cargo
UV ?= uv

.PHONY: fmt clippy test check guard test-tools

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
