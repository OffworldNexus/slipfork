---
id: M2.19
title: "Publish 0.2.0 DX docs, generated support matrix, CHANGELOG, dx and ai-ergonomics critics"
kind: task
status: ready
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
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:40"
---

# Goal

Promote every command and integration that actually shipped in `0.2.0` into lean, honest, published documentation, generate the support matrix from the registry, and record the `0.2.0` changelog entry — with `dx` and `ai-ergonomics` critics satisfied.

# Context

- DX-first rule: published pages may only describe behaviour that exists and runs today. `M2.1` wrote the target experience in `docs/design/target-dx.md`; this node is the promotion step, and nothing may be copied in from unshipped designs.
- `M2.15` created `tools/check_support.py` and `make support-sync`; this node is the only place where the released support table is regenerated from `support/integrations.toml`.
- Open documentation follow-ups: `M1.6` #14 to #16 and `M1.7` #13 and #14 (stale command names, duplicated manifest descriptions, pages drifting past a readable length, the manifest unknown-field claim, and the benchmark table promise).
- `zensical.toml` drives `make docs-check`, which fails on broken links and on any command that the honesty checker cannot find in the binary surface.

# Design / Invariants

1. **Only shipped surface.** Every command page section is derived from the real `--help` output and one verified example. A command that is still gated (tunnel sharing while the registrar is unavailable) is documented as unavailable with its exact error, never as a working feature.
2. **Lean pages.** Each page in `docs/` stays at or below 160 lines, with one example per command and no duplicated manifest reference (single canonical page, others link to it). `README.md` contains the install path, one end-to-end example, and links only.
3. **Support registry promotion.** Integrations that are user-reachable in `0.2.0` move to `declared` in `support/integrations.toml` with a real `recipe_page` and a real `critical_test` path; everything still internal stays omitted per `H-009` Option B. Changes are made through the documented mutator or an explicitly reasoned edit, never by hand-tweaking the generated table.
4. **Generated matrix.** `docs/support-matrix.md` is produced by `make support-sync` and committed; a manual edit fails `tools/check_support.py --check`.
5. **Changelog.** `CHANGELOG.md` gets one `0.2.0` section listing the added commands, the protocol and engine changes, and the removals (the deleted REST sketch), with no compatibility promises since the workspace is pre-`1.0`.
6. **Critics.** Record `docs/design/m2-docs.review-dx.md` and `docs/design/m2-docs.review-ai-ergonomics.md`: every command must have a copy-pasteable invocation, an obvious failure mode, and no ambiguous target selection. Unresolved findings block this node.
7. `make docs-check` runs Zensical in strict mode plus `tools/check_docs_honesty.py`; both must exit `0`.

# Acceptance

```bash
# 1. Every published page stays lean.
for f in docs/index.md docs/getting-started.md docs/snow-modes.md docs/testing-dag.md docs/agents-and-opencode.md docs/support-matrix.md; do test $(wc -l < "$f") -le 160 || exit 1; done

# 2. Honesty, links, and the generated matrix.
uv run --no-project tools/check_docs_honesty.py
make docs-check
uv run --no-project tools/check_support.py --check
git diff --exit-code docs/support-matrix.md

# 3. Changelog and critics were recorded.
rg -q 0.2.0 CHANGELOG.md
ls docs/design/m2-docs.review-dx.md docs/design/m2-docs.review-ai-ergonomics.md

# 4. Full gate.
make check
```

All commands exit `0`. Adversarial assertions required: a hand-edited `docs/support-matrix.md` makes `--check` fail, a page describing an unshipped flag fails the honesty checker, and no published page exceeds 160 lines.
