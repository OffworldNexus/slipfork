---
id: M3.35.4
title: "Multi-service polyglot E2E stories & GitHub Actions multi-platform workflows"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.35.3]
paths:
  - ".github/**"
  - "support/**"
  - "poc/**"
  - Makefile
  - "crates/slipfork/tests/**"
  - "crates/slipfork-engine/tests/**"
  - "packages/opencode-slipfork/tests/**"
  - "tools/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-20T16:14"
updated: "2026-11-21T00:43"
---

# Goal

Prove every `TESTING_PLAN.md` §2–§6 user story end-to-end with real polyglot runtimes and wire CI from scratch: `.github/` **does not exist today**, so this slice creates the workflow surface that covers Linux `x86_64` + `aarch64`, polyglot apps, install/checksum refusal, CoW fork, OSPP and the OpenCode plugin.

# Context

- **No `.github/` directory exists** (verified: `find .github` -> absent). Create `.github/workflows/{ci.yml,btrfs-live.yml,release-archives.yml,opencode-plugin.yml}`. Do not add YAML meta-linters back (M3.35.1 deleted `critical_guard.py`; upstream `actions/*` + `Swatinem/rust-cache` are fine).
- **Polyglot §2 fixtures live under `support/`** next to the existing `support/demo`: add `support/e2e/polyglot/` with (a) Python service (FastAPI/Flask shape) + (b) Node/TypeScript + static SPA, (c) PHP service, all behind one `.localhost` origin with `/api` path + subdomain routes, real `postgres:18`, `redis:7`, and an S3-compatible (MinIO) endpoint declared as resources; HMAC/secret values via `secret(...) ?? <generator>`. Assert live `$UID:$GID` HMR bind mount + isolated CoW `volume` submount, `setup`/deploy hook failure log tails, and `slipfork run`.
- **§3 install/readiness**: `x86_64` + `aarch64` (`gnu`/`musl`) archives + `install.sh` checksum-mismatch refusal (assert non-zero exit and zero files installed), `doctor` 3-row fail-closed with exact remedies, `sudo slipfork daemon install` serving `/` on `:80` with port-less URLs and `demo` left running, `migrate` from legacy `snow.yml`.
- **§4/§5**: `--json` parity, `NO_COLOR` purity, `logs -f` across restart, `secret` hidden prompt, CoW `fork` divergence + measured millisecond latency, `drop` dirty refusal, `prune` reaping, `@slipfork/opencode` provisioning/routing/cleanup.
- **§6 OSPP v1**: `auth` PoW + P-256 PoP, WS subscribe matched on Git remote, `hpke-p256` in-memory decrypt + `env_updated` restart, encrypted seed dump -> `base` hydration, fail-closed origin/DNS-rebinding/UID refusals. Use the minimal `127.0.0.1:0` loopback reference server from `slipfork-ospp-testkit` — never a mock.
- **GHA substrate**: `btrfs-live.yml` must build a real Btrfs substrate on the runner (loop file + `mkfs.btrfs` + mount with `sudo`) and run `make rs-test-live` serialized, so Btrfs stories are CI-verified without the droplet; keep `tools/do_vm.py` for the `165.232.86.174` gate owned by M3.35.5.
- `poc/run_benchmark.py` + `poc/bench-baseline.json` remain the CoW latency oracle: extend the fork-latency story to read the same baseline numbers instead of hardcoding a second threshold.

# Acceptance

- `for f in .github/workflows/*.yml; do uv run --no-project --with pyyaml python -c 'import sys,yaml;yaml.safe_load(open(sys.argv[1]))' "$f"; done` (all parse)
- `uv run --no-project tools/guard.py && uv run --no-project tools/plan.py validate`
- `cargo test -p slipfork --tests --no-run && cargo test -p slipfork-engine --tests --no-run`
- Local: `make check` exits `0`; `uv run --no-project tools/do_vm.py sync && uv run --no-project tools/do_vm.py test`
- Every §2–§6 story id from `TESTING_PLAN.md` is referenced by at least one `e2e_*` test name and one workflow job (grep the workflow names in your final message).
