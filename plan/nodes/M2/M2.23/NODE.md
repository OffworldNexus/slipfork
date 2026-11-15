---
id: M2.23
title: "Purge stale M1 manifest/snow-mode docs after M2.4/M2.5 deletions"
kind: task
status: ready
owner: "doc-writer"
priority: 3
depends_on: [M2.4, M2.5]
paths:
  - docs/reference/manifest.md
  - docs/reference/cli.md
  - "docs/snow-modes.md"
  - docs/index.md
  - "docs/getting-started.md"
  - "docs/agents-and-opencode.md"
  - README.md
  - "docs/design/m2-manifest-docs.review-dx.md"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T19:11"
updated: "2026-11-15T19:18"
---

# Goal

Purge the now-false M1 statements from the two published reference pages before `0.2.0` ships: `docs/reference/manifest.md` still presents `slipfork_snow::OsppBundle` / the retired REST bundle wire and `TopLevelEnvsUnsupported`, and `docs/snow-modes.md` still documents the retired bundle format and the pre-M2 unknown-component rule. Deleted types must not be described as current, and the single canonical `WorldOrigin`/`effective_components` story must be stated once.

# Context

- `M2.4` deleted `OsppBundle`/`MemorySecretProvider::from_bundle`, collapsed the top-level-key error to `SnowError::ForbiddenTopLevelKey { key }`, added `SnowFile::effective_components` (`disabled: true` removal), and migrated `ResourceCoordinates::app_url` to `WorldOrigin::local(&route, 80)` (deleting `LocalHostRoute::url()`).
- `M2.5` deleted the `incus-engine` `tier = "none"` placeholder and unified `EvidenceField`, so any support-tier prose in these pages must not promise the old table.
- Published pages are honesty-checked by `uv run --no-project tools/check_docs_honesty.py` and `make docs-check`; this node keeps each page lean (<= 160 lines).

# Design / Invariants

1. **Delete, do not deprecate.** Every mention of `OsppBundle`, `from_bundle`, the REST `GET .../bundle` wire, `TopLevelEnvsUnsupported`, and `LocalHostRoute::url()` is removed from both pages. The bundle story points at `ADR-0008` (WebSocket `EnvBundle`) in one sentence, with no wire payload restated.
2. **Single canonical statements.** Top-level `envs:` rejection is documented as "unknown/forbidden top-level key names the key only" (no value echoed); mode-scoped component removal is documented only as `disabled: true`; `app_url` is documented as the world's single `WorldOrigin` URL (portless at `:80`, `:<port>` otherwise).
3. **Daemon-owned commands are honestly unavailable.** `slipfork fork`, `exec`, and `drop` fail closed in this transitional tree (they are served by `slipforkd` from `M2.16`/`M2.17`); every page that shows them (`docs/index.md`, `docs/getting-started.md`, `docs/agents-and-opencode.md`, `docs/reference/cli.md`) states that truth with the exact diagnostic and does not present them as working examples. Delete or clearly mark the copy-pasteable M1 examples; no page may claim a capability the binary refuses.
4. **No new prompts.** Examples stay copy-pasteable against the real `0.1.0`/`0.2.0` CLI surface; no command that is still unimplemented is presented as working.
4. Critics: record `docs/design/m2-manifest-docs.review-dx.md` (dx lens) if any user-facing wording changes beyond deletions.

# Acceptance

```bash
# 1. Retired names are gone from both published pages.
! rg -q 'OsppBundle|from_bundle|TopLevelEnvsUnsupported|LocalHostRoute::url' docs/reference/manifest.md docs/snow-modes.md

# 2. The canonical stories are present.
rg -q 'disabled: true' docs/reference/manifest.md
rg -q 'ADR-0008' docs/reference/manifest.md

# 3. Pages stay lean and the site builds strictly.
test "$(wc -l < docs/reference/manifest.md)" -le 160
test "$(wc -l < docs/snow-modes.md)" -le 160
uv run --no-project tools/check_docs_honesty.py
make docs-check
```

Every command must exit `0`.
