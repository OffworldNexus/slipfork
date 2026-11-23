---
id: M3.41.1
title: "Docs site scaffold, core pages, README/CHANGELOG & docs gate"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths:
  - Makefile
  - zensical.toml
  - README.md
  - CHANGELOG.md
  - docs/index.md
  - "docs/getting-started.md"
  - docs/install.md
  - docs/forkfile.md
  - docs/cli.md
  - "docs/javascripts/**"
  - "docs/stylesheets/**"
  - tools/check_docs_honesty.py
  - tools/tests/test_tools.py
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-21T16:35"
updated: "2026-11-21T16:40"
---

# Goal

Ship the problem-first public docs site and its mechanical gate: `zensical.toml`, `README.md`, `CHANGELOG.md`, the five core `docs/` pages, the custom `Forkfile` highlighter, and `make docs-check` / `make docs-serve`.

# Context

- Nothing exists yet: no `docs/`, `README.md`, `CHANGELOG.md`, `zensical.toml`. `Makefile:16` `.PHONY` and `Makefile:44-53` (`packages-check`, `guard`, `test-tools`) are the anchors to extend; keep `lsp-assets` (`Makefile:55`) untouched.
- `tools/check_docs_honesty.py:20-40`: `FENCE_RE`/`CMD_LINE_RE`/`INLINE_TABLE_CMD_RE` reject any `slipfork <verb>` in `README.md` + published `docs/**/*.md` that `slipfork --help` does not list; `EXCLUDED_PREFIXES` skips `design/ decisions/ research/ testing/` and `VISION.md`. Docs absent today => it no-ops. Only extend its docs-path detection for the new `docs/` tree; never weaken the verb check.
- Zensical 0.1.2 is available offline and `uvx --offline zensical build --strict` is verified working. Copy the 1:1 shape from `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0/zensical.toml`: `site_url = "https://offworldnexus.github.io/slipfork/"`, `docs_dir = "docs"`, nav as ordered `{Title = "page.md"}` entries, `pymdownx.superfences` with a `mermaid` `custom_fences` entry, `validation.links.anchors = true` + `validation.links.not_found = warn/relative`, custom palette, `extra_javascript = ["javascripts/forkfile-highlight.js"]`.
- Nav contains ONLY: `index.md`, `getting-started.md`, `install.md`, `forkfile.md`, `cli.md` (reference pages arrive in M3.41.2). Exclude `specs/`, `design/`, `decisions/`, `research/`, `knowledge/`, `testing/`, `archive/`.
- Ship surface truth: verbs are exactly those in `slipfork --help` (`crates/slipfork/src/cli.rs`). Never document `slipfork prune|share|unshare|run|lsp`; `slipfork-lsp` is a standalone multi-platform binary, not a verb. Ground copy in `specs/01-forkfile.md` (language), `specs/02-cli-ux.md` (visual trees/daily verbs), `specs/06-distribution-and-docs.md:175-195`.
- There is no `LICENSE` file; the Makefile `lsp-assets` notice is the only license text on disk today — do not invent a license.

# Acceptance

- `zensical.toml` exists with the 5-page nav, mermaid superfence, anchor+link validation, palette, and `extra_javascript` highlighter; no excluded trees in nav.
- `docs/index.md`, `getting-started.md`, `install.md`, `forkfile.md`, `cli.md`: each opens with user value plus a working copy-pasteable `# forkfile: 0.1` or `slipfork` snippet inside the first 25 lines, has >=1 Mermaid diagram, `!!!` admonitions, <=2-sentence paragraphs, and zero vaporware verbs.
- `README.md`: problem-first, `curl -fsSL https://…/install.sh | sh` + `slipfork up` in the first 25 lines, links only to real pages.
- `CHANGELOG.md`: single `0.1.0` Keep-a-Changelog section.
- `docs/javascripts/forkfile-highlight.js` registers a Forkfile language + WCAG-AA-contrast CSS under `docs/stylesheets/**`.
- New Makefile targets: `docs-check` = `uvx --offline zensical build --strict` then `uv run --no-project tools/check_docs_honesty.py`; `docs-serve` = `uvx --offline zensical serve`. Add to `.PHONY`.
- `make docs-check` exits 0; `make test-tools` exits 0.
