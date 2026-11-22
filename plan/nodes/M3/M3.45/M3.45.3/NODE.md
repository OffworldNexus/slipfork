---
id: M3.45.3
title: "Demo /home/slipfork/demo + feat-auth refresh, probe cleanup & final live proof"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.45.2]
paths:
  - "crates/**"
  - "tools/**"
  - "docs/**"
  - README.md
  - "specs/03-under-the-hood.md"
  - specs/README.md
  - "specs/01-forkfile.md"
  - TESTING_PLAN.md
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-21T22:40"
updated: "2026-11-22T03:18"
---

# Goal
Leave the demo host in its published state: `/home/slipfork/demo` with the public-read S3 + `static-cdn` story and `feat-auth` both live behind `:80` on a clean VM, with the real `< 1.0s` fork timing and TUI output captured from the shipped binary.

# Context
- Same host `165.232.86.174`; drive it with `tools/do_vm.py ssh` (do_vm.py:2281) and `sync` (do_vm.py:2105). This slice starts only after M3.45.2 proved all seven suites green on this exact `.deb`.
- `/home/slipfork/demo` Forkfile must carry `s3 assets { policy public-read; cdn static-cdn }`; verify the ingress serves `http://static-cdn.<world>.localhost` anonymously (`200` `GET`, `403` `PUT`). `feat-auth` stays live on `:80` alongside `demo`.
- Capture the real demo: run `slipfork fork <name>` inside `/home/slipfork/demo` on the interactive TTY, record the timing and the rendered frozen pane (Braille + 16-cell bar + rolling tail from `crates/slipfork/src/boot/pane.rs:12-22`), then remove every probe fork so `slipfork status` lists exactly `demo` + `feat-auth`.
- Docs honesty: `README.md` and `docs/**` may describe only what this run actually showed (`tools/check_docs_honesty.py`, `make docs-check`). If measured UX differs from the published copy, fix the copy or report the gap in your final message; never soften a claim by moving it into `specs/` only.
- Honesty: zero `RESULT.md`; quote commands and raw output. Leave no probe world, leftover fork, or stray systemd override behind.

# Acceptance
- `slipfork status` on the VM lists `demo` and `feat-auth` only, both `running` on `:80`; `curl -sS -o /dev/null -w '%{http_code}' http://demo--<world>.localhost/` (and the `feat-auth` equivalent) return `200`.
- Anonymous `curl` to `http://static-cdn.<world>.localhost/<key>` returns `200`; an anonymous `PUT` to the same key returns `403`.
- A fresh `slipfork fork <probe>` in `/home/slipfork/demo` completes, is measured `< 1.0s`, then is removed; the quoted wall time comes from that run, not from slice 2.
- Captured TUI bytes show the frozen pane (Braille spinner, 16-cell bar, rolling tail); under `NO_COLOR=1`/`TERM=dumb` the capture contains zero `\x1b[`.
- `uv run --no-project tools/check_docs_honesty.py` exits 0 and `make docs-check` exits 0 after any `README.md`/`docs/**` edit.
- `uv run --no-project tools/do_vm.py status` shows the droplet still up and tagged.
