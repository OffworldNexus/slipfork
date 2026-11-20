---
id: M3.40
title: "OSPP v1 cloud clutch, stateless :80 .localhost ingress, single per-user daemon, colorful CLI UX & @slipfork/opencode"
kind: epic
status: ready
priority: 3
depends_on: [M3.39]
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - ARCHITECTURE.md
  - "crates/slipfork-ospp/**"
  - "crates/slipfork-edge/**"
  - "crates/slipfork/**"
  - "packages/opencode-slipfork/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T00:44"
updated: "2026-11-21T00:44"
---

# Goal

Implement `specs/02-cli-ux.md`, `specs/03-under-the-hood.md` §4, and `specs/05-ospp-v1.md` cleanly end-to-end:
1. **`crates/slipfork-ospp`:**
   - Complete `ospp-v1` WebSocket client (`version: 2`, P-256 device keypair auth, ALTCHA+PoP `#handle=` pairing saved as public `{endpoint, account, machine}` in `<state_dir>/ospp.json`, Git-remote `subscribe` sending evaluated `Forkfile`, in-memory `hpke-p256` secret decryption + live `env_updated` service restarts, HTTPS `dump_ready` encrypted DB dump streaming) + local `0600` offline secret vault + `127.0.0.1:0` loopback reference OSPP server for E2E stories.
2. **`crates/slipfork-edge` & Single-Binary `crates/slipfork` Daemon/Proxy/CLI:**
   - **Stateless `:80` `.localhost` HTTP + WebSocket ingress proxy** (`sudo slipfork daemon install` installing `systemd` socket-activated `:80` proxy + `/etc/apparmor.d/slipfork` userns profile, streaming request bodies without head-of-line upload deadlocks), forwarding `<world>.localhost`, `<domain>.<world>.localhost`, `<prefix>.*`, `<cdn>.<world>.localhost`, and `<fork>--<world>.localhost` to the user daemon with DO-aligned `route` matching (keep-prefix default, `strip-prefix`, `rewrite`, `exact`).
   - **Single per-user daemon** (`$XDG_RUNTIME_DIR/slipfork.sock`) owning all worlds/forks under `$UID` (zero root/user split-brain, zero separate `slipforkd` binary).
   - **High-craft colorful CLI (`specs/02-cli-ux.md`):**
     - Bare `slipfork` (`status`) & scoped `ps` rendering the dynamic-width colored tree (World -> Services/Resources with human names, `:8000 -> /api`, `s3 (private)` vs `s3 + cdn -> <cdn>.*`, human ages -> nested Forks with child services; `○ No worlds running` exit `0`).
     - `up`, `restart [service]`, `down` with clean TTY spinners, timings, `(up to date)` badges, and inline red failure tail + `Forkfile:<line>` + `next:` remedy while keeping failed worlds running.
     - `fork [name]` (`< 50ms` snapshot summary) & `drop <name> [-f]`, `logs [-f] [service]` (colored gutters, chronological merge, `── restarted ──` divider across restarts), `shell [service|image]`, `exec [service|image] -- <cmd>`, `db [database]`, `redis [id]`, `env [service] [--reveal]`, `secret list|set|unset`, `open [service]` (SSH/headless-safe), `doctor` (3 crisp rows: Btrfs, Namespaces, Runtime `crun` + `:80`), `auth <url>`, `migrate`, `fmt`, `lsp`, `completions <shell>`.
     - Strict terminal hygiene: zero `\x1b[` or spinner garbage under `NO_COLOR`, `TERM=dumb`, pipes, or `--json`.
3. **`packages/opencode-slipfork`:**
   - OpenCode V2 plugin (`ctx.worktree.transform` + `ctx.shell.hook` + `ctx.session.hook`) creating/reaping `slipfork fork` / `drop` worlds automatically for parallel AI coding agents.

# Acceptance

- `make check` exits `0` across all Rust crates and `packages/opencode-slipfork`.
