---
id: M3.33
title: "Author TESTING_PLAN.md pure user-story testing vision"
kind: task
status: superseded
owner: "doc-writer"
priority: 3
depends_on: [M3.32]
paths: [TESTING_PLAN.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-20T16:12"
updated: "2026-11-21T00:43"
---

# Goal

Write `TESTING_PLAN.md` (~60–75 lines) at the repository root as Slipfork's permanent, pure user-story testing vision — modeled directly on `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0/TESTING_PLAN.md` (`0493486`).

# Strict Format & Anti-Code-Leakage Rules

- **ZERO code, file, crate, module, struct, trait, ioctl, or plan-ID references** anywhere in `TESTING_PLAN.md` (ban `crates/*`, `.rs`, `BtrfsEngine`, `WorldEngine`, `ServicePod`, `ADR-*`, `M3.*`, `SIOC*`, etc.).
- Write **100% from the user's point of view**: what business values, workflows, developer outcomes, and security/isolation/fail-closed guarantees Slipfork promises in `README.md` and `docs/**`.
- Describe strictly shipped `0.1.0` capabilities (zero `0.2.0` `slipfork test`/STP v1 or `slipfork share` vaporware; verify against `README.md`, `docs/**`, `editors/zed`, and `crates/slipfork-lsp`).
- Keep concise (`~60–75` lines total) across 7 scannable sections:
  1. `## 1. Testing Philosophy` — E2E through real user workflows (real polyglot services, PostgreSQL/Redis/S3-compatible containers, Btrfs Copy-on-Write subvolumes, `.localhost` HTTP/WebSocket ingress, `git` worktrees, and the shipped `slipfork` binary) proving both the expected user outcome and the isolation/secret-safety/fail-closed boundary; focused non-E2E tests exist **only** for §7's self-contained language, wire/crypto protocol, and editor surfaces; *"No test exists unless it protects one of the user stories below."*
  2. `## 2. Polyglot Applications, Services & Backing Resources (Forkfile)` — Multi-service polyglot apps (Python, Node/TypeScript, PHP, static/SPA) in per-service OCI containers from `Dockerfile` stages and upstream images behind one `.localhost` origin (`/` + `/api` path routes, subdomain routes, WebSockets, `mail.<project>.localhost`); managed `postgres` (default 18), `redis` (default 7), and S3-compatible storage on shared world loopback; `features` (`--features`, `--no-default-features`, `on <expr>`), `env` groups, and `secret(...) ?? <generator>` values never written to persistent disk, logs, or process command lines; live `$UID:$GID` checkout bind mounts for HMR/`inotify` alongside isolated Copy-on-Write `volume` submounts, `setup` & deploy hooks (preserving failed worlds with inline log tails), and `slipfork run <task>`.
  3. `## 3. Installation, Host Readiness & Migration` — Standalone Linux `x86_64` & `arm64` archives and `install.sh` (rejecting tampered archives on checksum mismatch); 3-row `slipfork doctor` readiness check (Btrfs Copy-on-Write round-trip, unprivileged namespaces + Ubuntu AppArmor profile, `crun` runtime) failing closed with exact remedies; socket-activated `:80` `.localhost` ingress (`sudo slipfork daemon install`, port-less URLs) + ephemeral user-space fallback; lossless `slipfork migrate` from legacy `snow.yml`.
  4. `## 4. Local Development & Fleet Operations` — `slipfork up`, `restart`, `down`, bare `slipfork` (`status`) World -> Services -> Forks tree, `ps`, `--json` parity, `NO_COLOR`/non-TTY purity; chronological `stdout+stderr` `logs -f` surviving `restart`, per-service `shell` and `exec`, `env`, interactive & piped `db` and `redis`, hidden-prompt `secret set`/`unset`/`list`, port `forward`, `hook` re-runs, and SSH-aware `open`.
  5. `## 5. Instant Copy-on-Write Branching & AI Coding Agents` — Millisecond Copy-on-Write world + `git` worktree forks (`slipfork fork [name]`) with full PostgreSQL/Redis/`volume` divergence and CoW latency proof; `drop` dirty-worktree refusal (`--force` override), `down --all`, and `prune` reaping all containers and process groups without leaked mounts/subvolumes; zero-config `@slipfork/opencode` worktree provisioning, cwd routing, and session cleanup.
  6. `## 6. Remote Environment Clutch & Zero-Trust Secrets (OSPP v1)` — `slipfork auth <url>` PoW + P-256 PoP pairing and Git-remote-matched WebSocket subscriptions; in-memory `hpke-p256` secret decryption with live `env_updated` service restarts; encrypted remote seed dumps hydrated into the reusable `base` snapshot (`--no-dump` / `--refresh-dump`); fail-closed `.localhost` / control-socket origin, DNS-rebinding, and UID refusals.
  7. `## 7. Language, Wire Protocol & Editor Tooling (Focused Coverage)` — `Forkfile` syntax, whole-file multi-error static checker across all `on` branches (`line:col` caret diagnostics + secret-taint enforcement), idempotent comment-preserving formatting, `snow.yml` conversion, and runnable doc `Forkfile` fences + client-side syntax highlighter; local daemon RPC framing and `OSPP v1` WebSocket/`hpke-p256` framing & handshake; `slipfork lsp` (UTF-16 diagnostics, hover, formatting), Tree-sitter grammar, and Zed extension.

# Acceptance

- `TESTING_PLAN.md` exists (`55–80` lines), contains zero file paths, crate/struct names, or `M3.*`/`ADR-*` jargon, reflects 100% of shipped `0.1.0` user docs honestly, and passes `uv run --no-project tools/guard.py`.
