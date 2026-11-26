---
id: M3.186
title: "Makefile hardcodes target/debug/slipfork: make check breaks when CARGO_TARGET_DIR is redirected"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [Makefile, "scripts/**", docs/install.sh]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T22:12"
updated: "2026-11-25T22:45"
---

# Problem

`make check`'s JS/packages legs run with a hardcoded `target/debug/slipfork`, so any run with a redirected `CARGO_TARGET_DIR` (the documented agent-build pattern, and CI cache layouts) fails or needs a temporary symlink. `M3.182.3` had to bridge it manually.

# Fix

- Resolve the binary from `${CARGO_TARGET_DIR:-target}/debug/slipfork` in every Makefile/script reference (or a single `SLIPFORK_BIN ?=` variable used everywhere).
- Keep the default identical when `CARGO_TARGET_DIR` is unset.
- Do not edit `docs/` (owner editing); `scripts/**` and `Makefile` only.

# Acceptance

- `CARGO_TARGET_DIR=/var/tmp/x-target make check` passes end-to-end with no symlink, and plain `make check` is unchanged.
- `make live-test`/`scripts/*` references consistent; `guard.py` green; commit `[M3.186] ...`, done, no push.
