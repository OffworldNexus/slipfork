---
id: M3.41.2
title: "Reference docs: under-the-hood, OSPP, agents/OpenCode, troubleshooting"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: [M3.41.1]
paths:
  - "docs/under-the-hood.md"
  - docs/ospp.md
  - "docs/agents-and-opencode.md"
  - docs/troubleshooting.md
  - zensical.toml
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-21T16:35"
updated: "2026-11-21T16:45"
---

# Goal

Add the four visual reference pages that explain how Slipfork actually works, and wire them into the Zensical nav.

# Context

- Depends on M3.41.1, which creates `zensical.toml` with a 5-page nav (`index`, `getting-started`, `install`, `forkfile`, `cli`) and `docs/javascripts/forkfile-highlight.js`. Append exactly four nav entries after the core five: `under-the-hood.md`, `ospp.md`, `agents-and-opencode.md`, `troubleshooting.md` — total 9 pages, no other nav churn.
- Sources of truth (cite behavior only from these): `specs/03-under-the-hood.md` (Btrfs subvolume layout, native image engine, per-service `crun` pods, boot DAG, single per-user daemon, `:80` ingress), `specs/05-*` OSPP wire protocol (pairing, HPKE secrets, sealed dumps, stateless ingress, 12 error codes), `specs/06-distribution-and-docs.md:196-229` §11 (`@slipfork/opencode` transform/shell/idle hooks, `SLIPFORK_WORLD`, `SLIPFORK_URL`), `specs/02-cli-ux.md` (doctor output, daemon install, `--ingress`, AppArmor, non-Btrfs failures).
- Ground CLI text in `crates/slipfork/src/install.rs:33-95` (`USER_UNIT`, `INGRESS_SOCKET` on `127.0.0.1:80` with `Accept=yes`, `INGRESS_SERVICE`, `APPARMOR_PROFILE`, `SOCKET_UNIT`=`slipfork-ingress.socket`, `SERVICE_UNIT`=`slipfork-ingress@.service`). Never invent flags.
- `tools/check_docs_honesty.py:20-40` scans these pages: every `slipfork <verb>` must exist in `slipfork --help`. `slipfork-lsp` is standalone; no `slipfork lsp` verb. `slipfork fork <name>` / `slipfork drop <name>` are the agent-world verbs.

# Acceptance

- `docs/under-the-hood.md`: Mermaid diagram of the per-world Btrfs subvolume tree + boot DAG; explains native image engine (no Docker), `crun` pods, single daemon and socket-activated `:80` ingress; a scannable table of engine responsibilities.
- `docs/ospp.md`: pairing handshake, HPKE secret handling, sealed dumps, stateless ingress, and a 12-row error-code table with remedy column.
- `docs/agents-and-opencode.md`: transform / shell / idle hook table, `slipfork fork <name>` + `slipfork drop <name>` snippets, `SLIPFORK_WORLD` / `SLIPFORK_URL` env table.
- `docs/troubleshooting.md`: each entry shows the real `message` and a one-line `next:` remedy (`doctor`, `daemon install`, `--ingress` + AppArmor, non-Btrfs host); zero vaporware or "coming soon".
- Every page: value-first opening, >=1 Mermaid diagram, `!!!` admonitions, <=2-sentence paragraphs; no page exceeds the docs voice budget.
- `make docs-check` exits 0 with all 9 pages resolving (strict links + anchors).
