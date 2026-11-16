---
id: M3.2.17
title: "0.3.0 release prep: workspace version, CHANGELOG breaking note, docs nav label"
kind: task
status: ready
owner: implementer
priority: 4
depends_on: [M3.2.16]
paths: [CHANGELOG.md, Cargo.toml, Cargo.lock, zensical.toml]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Prepare the `0.3.0` release: workspace version bump, `CHANGELOG.md` entry describing the Btrfs-only wipe plus `slipfork test`, and the docs nav label — with everything still green and nothing published ahead of the code.

# Context

- `0.2.0` shipped the single-origin edge; `0.3.0` ships the Btrfs-only engine rewrite **and** the polyglot test orchestrator, so the changelog must state the breaking deletion of prior engine adapters plainly (SemVer honesty, `review-cruft`).
- Version lives in `[workspace.package] version`; no per-crate versions exist.

# Design / Invariants

1. `Cargo.toml`: `version = "0.3.0"` under `[workspace.package]`; run `cargo check --workspace` so `Cargo.lock` records the new version; never hand-edit the lockfile.
2. `CHANGELOG.md`: a `## 0.3.0` section with **Breaking** (single Btrfs-only `WorldEngine`; every prior engine adapter/storage pool/`FICLONE` path deleted; Btrfs host prerequisite with the exact mount options), **Added** (`slipfork test`, warm pool, typed coverage, `pytest-slipfork`, `CommandTestAdapter`, `prune` GC, `daemon unit --only apparmor`), and **Fixed** (`make check` determinism). Written for users, not internals.
3. `zensical.toml`: nav label `"CLI (0.3.0)"` (and any other version-labelled entry) updated; no other site config change.
4. No doc claims a feature that is not in the tree; no `[Unreleased]` leftovers and no duplicate version heading.

# Acceptance

```bash
grep -n 'version = "0.3.0"' Cargo.toml
grep -n '## 0.3.0' CHANGELOG.md
grep -n 'Breaking' CHANGELOG.md
grep -n '0.3.0' zensical.toml
cargo check --workspace
make docs-check
make check
```

Expected: workspace version and changelog heading are `0.3.0`, the breaking note exists, the nav label is updated, and `cargo check --workspace`, `make docs-check`, and `make check` all exit 0.
