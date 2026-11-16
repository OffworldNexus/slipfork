---
id: M1.6
title: "Fold validated .localhost edge, weaved IPC, git/jj remote & polyglot test protocol into M1 design/code"
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M1.5]
paths:
  - Cargo.toml
  - Cargo.lock
  - ARCHITECTURE.md
  - README.md
  - zensical.toml
  - "docs/**"
  - "crates/**"
  - "packages/**"
  - plan/ROADMAP.md
attempts: 3
max_attempts: 3
escalation: architect
created: "2026-11-14T23:16"
updated: "2026-11-15T02:01"
---

# Goal

Fold owner's Milestone 1 design refinements cleanly across user docs, `ARCHITECTURE.md`, ADRs, `ROADMAP.md`, and `slipfork-snow` (zero `.localtest.me` residue):

1. **Port-80 `*.localhost` Edge Daemon (`slipforkd`):** Route `http://(<snow-domain>.)?(<fork>.)?<project>.localhost` on port 80 into isolated worlds, forbidding dots and `snow.yml` `domains[].id` collisions in fork names so 1/2/3-label URLs are 100% unambiguous; replace `.localtest.me` with `.localhost` everywhere in `crates/slipfork-snow` and `docs/`.
2. **Tunnel Weaver Daemon IPC (`weaved`):** No crate/binary hard-linking — define a clean `TunnelRegistrar` DI trait in `ARCHITECTURE.md` & `ADR-0005` talking over local IPC Unix socket to the Tunnel Weaver daemon (`weaved`), using connection/stream lifetime as the tunnel lease.
3. **Developer & VCS Ergonomics:** Document and architect `slipfork shell [<fork>]`, `git-remote-slipfork` (`git pull slipfork <fork>` + `wip` ref), and native Jujutsu (`jj workspace`) behind a `VcsWorkspace` DI trait.
4. **Language-Agnostic Test Orchestrator (`ADR-0006`):** Ensure `slipfork-test` is NOT coupled to or limited by `pytest`: define a universal JSON/Unix-socket `RunnerProtocol` + the 5 Speed/Stability Conditions (pre-warmed CoW world pool + shared host Chromium `BrowserContext`s over `http://test-<slot>.<project>.localhost`, two-tier `batch` vs `world` scheduling, O(1) loopback-XFS/ZFS volume cloning, orthogonal artifact/fixture DAG, deterministic `CHECKPOINT` + readiness sealing). Start with `pytest` (`pytest-slipfork`) as Adapter #1 in M3 while proving polyglot generality via a shared `TestAdapter` conformance suite.

# Acceptance

- [ ] `rg -n "localtest\.me" docs crates README.md ARCHITECTURE.md` returns 0 matches (exit 1).
- [ ] `make check` exits 0 (including updated `critical_snow.rs`, `zensical build --strict`, `guard.py`, `clippy -D warnings`).
