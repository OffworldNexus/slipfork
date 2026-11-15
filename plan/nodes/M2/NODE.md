---
id: M2
title: "Single-Origin .localhost Edge (slipforkd), weaved Leases, Live VCS Worlds & Support CLI (0.2.0)"
kind: milestone
status: ready
priority: 2
depends_on: [M1]
paths: []
attempts: 0
max_attempts: 3
escalation: strategist
created: "2026-11-14T22:03"
updated: "2026-11-15T11:46"
---

# Goal

Deliver Slipfork `0.2.0`: single-origin local & shared worlds (`WorldOrigin = Local { url } | Shared { url }` starting on `FileVaultProvider` without requiring OSPP), port-80 `slipforkd` sole ingress (`H-008`, `shared_hosts` map + `*.localhost`-scoped `308`/`421` rules), interactive lifecycle CLI (`up`, `down`, `shell`, `share`, `list`, `prune`, `secret set`) with mode-scoped `modes.<mode>.components.<id>` supervision, `VcsWorkspace` (`GitWorktreeAdapter` + `JjWorkspaceAdapter` + `git-remote-slipfork`), `slipfork support [--json]` + generated docs matrix & `make check` ceiling (`ADR-0007`, gated on `H-009`), live `WorldEngine` (`ReflinkNetnsEngine` + `IncusEngine` gated on `H-004`), `WeavedRegistrar` (gated on `H-006`), and optional later `OSPP v1` client leaf.

# Acceptance

- [ ] `H-009` (`ADR-0007` pre-`declared` conformance-tested adapter tier classification) resolved before `slipfork support [--json]` and the generated docs support matrix ship; `make check` enforces the read-only support ceiling (`git` commit attestation + CI workflow job verification) and `make support-sync` reports demotions explicitly.
- [ ] `slipforkd` enforces the single-origin invariant (`ADR-0005` §1–§2, `H-008`): `WorldOrigin` assigned before materialization without requiring OSPP, exact 1-URL `slipfork list` output, `shared_hosts` proxying via `dial_world_tcp`, and `*.localhost`-scoped `308`/`421` rules.
- [ ] `VcsWorkspace` (`Git` + `Jj` + `git-remote-slipfork`) passes `assert_vcs_workspace_conformance`; external host/wire items `H-004` (`incusd`) and `H-006` (`weaved` IPC spec) resolved for their respective live adapters.
- [ ] `make check` exits 0 and all milestone exit criteria in `plan/ROADMAP.md` (`M2`) verified.
