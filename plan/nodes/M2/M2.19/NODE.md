---
id: M2.19
title: "Publish 0.2.0 DX docs, generated support matrix, CHANGELOG, dx and ai-ergonomics critics"
kind: task
status: active
owner: "doc-writer"
priority: 3
depends_on: [M2.15, M2.17, M2.18]
paths:
  - README.md
  - CHANGELOG.md
  - zensical.toml
  - docs/index.md
  - "docs/getting-started.md"
  - "docs/snow-modes.md"
  - "docs/testing-dag.md"
  - "docs/agents-and-opencode.md"
  - "docs/support-matrix.md"
  - "docs/reference/**"
  - "docs/design/**"
  - support/integrations.toml
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T23:05"
---

<!-- splice: replace everything after the frontmatter of plan/nodes/M2/M2.19/NODE.md with this file. Frontmatter requires `depends_on: [M2.15, M2.17, M2.18, M2.29]` (apply with `uv run --no-project tools/plan.py edit M2.19 --depends M2.15,M2.17,M2.18,M2.29`) so `[world]`/`--host-caches`/container-scoped `db`/`redis` are only published once `M2.29` ships them. Source of truth: ADR-0009 §1–§3 + docs/design/world-base-image.md (`M2.25`). -->

# Goal

Promote every command and integration that actually shipped in `0.2.0` into lean, honest, published documentation, generate the support matrix from the registry, and record the `0.2.0` changelog entry — with `dx` and `ai-ergonomics` critics satisfied.

# Context

- DX-first rule: published pages may only describe behaviour that exists and runs today. `M2.1` wrote the target experience in `docs/design/target-dx.md`; this node is the promotion step, and nothing may be copied in from unshipped designs.
- `M2.15` created `tools/check_support.py` and `make support-sync`; this node is the only place where the released support table is regenerated from `support/integrations.toml`.
- **New surface awaiting promotion from `M2.24`–`M2.29` (`ADR-0009`):** `[world]` in `slipfork.toml` (`world.image` when `snow.yml` declares more than one `BuildImage`, `world.stage` for a compiled build stage), the fail-closed "no `BuildImage`" diagnostic, `--host-caches` (read-only, ABI-gated, never a `slipfork.toml` key), the 6-rule World Image Contract, and the fact that `slipfork db` / `slipfork redis` enter the `postgres:<v>` / `redis:<v>` container. `docs/design/world-base-image.md` §1–§5 is the internal source; **nothing** about `world.image`/`world.stage`/`crun` may appear in published pages before this node runs.
- Open documentation follow-ups: `M1.6` #14 to #16 and `M1.7` #13 and #14 (stale command names, duplicated manifest descriptions, pages drifting past a readable length, the manifest unknown-field claim, and the benchmark table promise).
- `zensical.toml` drives `make docs-check`, which fails on broken links and on any command that the honesty checker cannot find in the binary surface.

# Design / Invariants

1. **Only shipped surface.** Every command page section is derived from the real `--help` output and one verified example. A command that is still gated (tunnel sharing while the registrar is unavailable) is documented as unavailable with its exact error, never as a working feature.
2. **Lean pages.** Each page in `docs/` stays at or below 160 lines, with one example per command and no duplicated manifest reference (single canonical page, others link to it). `README.md` contains the install path, one end-to-end example, and links only.
3. **World base image promoted by reference, once (`ADR-0009`).** `docs/reference/manifest.md` gains the `[world]` table (two optional keys, `deny_unknown_fields`, inference rules, and the fail-closed "cannot be built into an image" message) and `docs/reference/cli.md` gains `--host-caches` plus container-scoped `db`/`redis`; the 6-rule contract and the distro table live **only** in `docs/design/world-base-image.md` and are linked, never restated. `docs/getting-started.md` gains the one-line `Dockerfile` + `images:` requirement and shows `[world]` only in the multi-image/compiled example.
4. **Support registry promotion.** Integrations that are user-reachable in `0.2.0` move to `declared` in `support/integrations.toml` with a real `recipe_page` and a real `critical_test` path; everything still internal stays omitted per `H-009` Option B. No entry may claim a tier the `Dockerfile` `L0` + `crun` topology contradicts (e.g. `reflink-netns-world-engine`'s recipe must show the image-backed world, not M1's `slipfork snap` shim). Changes are made through the documented mutator or an explicitly reasoned edit, never by hand-tweaking the generated table.
5. **Generated matrix.** `docs/support-matrix.md` is produced by `make support-sync` and committed; a manual edit fails `tools/check_support.py --check`.
6. **Changelog.** `CHANGELOG.md` gets one `0.2.0` section listing the added commands, the protocol and engine changes, the `Dockerfile`-derived `L0` rootfs + `crun` service containers, and the removals (the deleted REST sketch and M1's host-shared-`/usr` topology), with no compatibility promises since the workspace is pre-`1.0`.
7. **Critics.** Record `docs/design/m2-docs.review-dx.md` and `docs/design/m2-docs.review-ai-ergonomics.md`: every command must have a copy-pasteable invocation, an obvious failure mode, and no ambiguous target selection. Unresolved findings block this node.
8. **MSRV honesty.** The pinned upstream `tpmd-client`/`tpmd-ipc`/`tpmd-proto` require Rust `1.94`, so the published install path must say `Rust 1.94+` (never `1.88+`) everywhere it is stated; do not vendor, patch, or downgrade the `tpmd` crates to regain the old MSRV.
9. `make docs-check` runs Zensical in strict mode plus `tools/check_docs_honesty.py`; both must exit `0`.

# Acceptance

```bash
# 1. Every published page stays lean.
for f in docs/index.md docs/getting-started.md docs/snow-modes.md docs/testing-dag.md docs/agents-and-opencode.md docs/support-matrix.md; do test $(wc -l < "$f") -le 160 || exit 1; done

# 2. Honesty, links, and the generated matrix.
uv run --no-project tools/check_docs_honesty.py
make docs-check
uv run --no-project tools/check_support.py --check
git diff --exit-code docs/support-matrix.md

# 3. The ADR-0009 surface is published exactly where ADR-0009 puts it.
rg -q 'world\.image' docs/reference/manifest.md
rg -q 'world\.stage' docs/reference/manifest.md
rg -q -- '--host-caches' docs/reference/cli.md
rg -q 'postgres:<v>' docs/reference/cli.md
! rg -q 'World Image Contract #|6-Rule|curated base' docs/reference/*.md docs/getting-started.md

# 4. Changelog and critics were recorded.
rg -q 0.2.0 CHANGELOG.md
ls docs/design/m2-docs.review-dx.md docs/design/m2-docs.review-ai-ergonomics.md

# 5. Full gate.
make check
```

All commands exit `0`. Adversarial assertions required: a hand-edited `docs/support-matrix.md` makes `--check` fail, a page describing an unshipped flag fails the honesty checker, no published page exceeds 160 lines, and `docs/design/world-base-image.md` remains the only page that states the World Image Contract rules.
