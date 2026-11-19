---
id: M2.32
title: "Align 0.2.0 docs, CHANGELOG, M1 benchmark topology label, and support matrix with completed M2 surface"
kind: task
status: done
owner: "doc-writer"
priority: 2
depends_on: [M2.30, M2.31]
paths:
  - README.md
  - CHANGELOG.md
  - docs/index.md
  - "docs/snow-modes.md"
  - "docs/getting-started.md"
  - "docs/reference/**"
  - support/integrations.toml
  - "docs/support-matrix.md"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-16T03:04"
updated: "2026-11-16T06:04"
---

# Goal

Update published `0.2.0` documentation, `CHANGELOG.md`, and `support/integrations.toml` (`docs/support-matrix.md`) so every claim matches the completed `M2.30` + `M2.31` `0.2.0` binary and benchmark topology with zero inaccuracies or vaporware.

# Context

- M2 milestone gate honesty audit identified four documentation gaps to close once `M2.30` and `M2.31` land:
  1. `docs/reference/manifest.md:90` & `CHANGELOG.md:28` stated that `FileVaultProvider` is the only secret provider wired and remote `OSPP v1` `EnvBundle` resolution is un-wired; `M2.30` wires `LayeredSecretProvider<FileVaultProvider, OsppSecretProvider>` + streaming `pg_restore` seed dumps.
  2. `docs/reference/manifest.md:113` falsely claimed `project` defaults *"via `jj root` when `jj` is on PATH"* (`RepoRootResolver` resolves colocated Jujutsu checkouts through their Git store without invoking `jj`, while `M2.30` routes `.jj` `fork` workspaces to `JjWorkspaceAdapter`).
  3. `docs/index.md:36-47` and `README.md:28` quote M1's `44.4 ms` / `3.21x` (`118 ms`/test) numbers without labelling them as the **historical M1 host-shared-`/usr` topology baseline** (`docs/knowledge/LESSONS.md:16`; keep `docs/research/m1-poc-benchmark.md` byte-untouched so `poc/bench-baseline.json`'s `source_report_sha256` stays valid).
  4. `docs/index.md:59` and `README.md:49` listed *"remote `OSPP v1` secret bundles, Jujutsu workspaces"* under `"M2 (0.2.0, remainder)"`; with `M2.30` shipped, only external-gated items (`weaved` tunnel sharing `H-006`, live `IncusEngine` `H-004`) and `prune` rootfs/OCI cache GC remain deferred.

# Design / Invariants

1. **Manifest & `snow-modes` Accuracy (`docs/reference/manifest.md`, `docs/snow-modes.md`):**
   - Document `LayeredSecretProvider` (`0600` local vault read first + sole `!!` write sink in `local` mode; paired `OSPP v1` `env.bundle` read second when paired via `slipfork auth login`, failing closed on missing keys when unpaired) and container-restored `OSPP v1` PostgreSQL seed dumps (`--no-dump` / `--refresh-dump`).
   - Replace the false `via jj root` claim in `docs/reference/manifest.md` with the exact truth: canonical `ProjectSlug` resolves through the Git store (including colocated Jujutsu checkouts), and `slipfork fork` uses `JjWorkspaceAdapter` when `<vcs_root>/.jj` is present (`jj` required on `PATH`, zero silent fallback to Git).
2. **Historical M1 Benchmark Labelling (`docs/index.md`, `README.md`):**
   - Explicitly label the M1 `3.21x` / `44.4 ms` figures as the historical M1 **host-shared-`/usr` topology** baseline (`docs/research/m1-poc-benchmark.md`, pre-`ADR-0009`), leaving `docs/research/m1-poc-benchmark.md` untouched so `poc/bench-baseline.json` provenance holds.
3. **Roadmap Corner, `CHANGELOG.md` & Support Matrix (`support/integrations.toml`, `docs/support-matrix.md`):**
   - Remove shipped `OSPP v1` secret/dump capabilities and Jujutsu workspaces from `"not yet shipped"` in `docs/index.md` and `README.md`; update `CHANGELOG.md` `## 0.2.0` (`Added` / `Changed`) to reflect `LayeredSecretProvider`, container `pg_restore` + `freeze_world` base snapshots, `.jj` `JjWorkspaceAdapter` routing, and optional `slipfork.toml`.
   - Promote `ospp-v1-secrets-and-dumps` (and `jj-workspaces`) to `declared` in `support/integrations.toml` pointing at real recipe pages and `M2.30`'s protected `critical_*` tests, then regenerate `docs/support-matrix.md` via `make support-sync`.
4. **Leanness Ceiling:** Keep every published page in `docs/*.md` and `docs/reference/*.md` at `<= 160` lines.

# Acceptance

```bash
for f in docs/index.md docs/getting-started.md docs/snow-modes.md docs/testing-dag.md docs/agents-and-opencode.md docs/support-matrix.md docs/reference/cli.md docs/reference/manifest.md; do test $(wc -l < "$f") -le 160 || exit 1; done
! rg -q "via \`jj root\`" docs/ README.md
! rg -q "remote .?OSPP v1.? secret bundles" docs/index.md README.md
rg -q "host-shared" docs/index.md README.md
uv run --no-project tools/check_support.py --check
git diff --exit-code docs/support-matrix.md docs/research/m1-poc-benchmark.md
make check
```

All commands exit `0`.
