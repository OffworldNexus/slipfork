---
name: DX-First (High-Craft CLI & Visual User Documentation)
description: MANDATORY for planner, writer, implementer, architect, review-ux, lead, and strategist. Everything stems from Developer Experience: problem-first visual Zensical docs, rich ANSI/spinner CLI ergonomics, and zero published vaporware.
---

# DX-First: High-Craft CLI & Visual Documentation

Slipfork exists to make spinning up polyglot dev environments, forking agent worktrees, and running isolated stacks feel **instantaneous and invisible**. Every extra CLI flag, sheared table column, silent 10-second freeze, manual `.env` dance, or wall of lawyer prose is a defect.

## 1. Docs Before Code — Zero Published Vaporware

1. **Draft Pre-Code DX Blueprints in `docs/design/dx-<slug>.md` (excluded from public Zensical site):**
   - Never publish CLI commands, flags, or integrations in `docs/` or `README.md` that do not exist and run in the compiled `slipfork` binary (`uv run --no-project tools/check_docs_honesty.py` enforces this mechanically).
   - Draft unshipped target UX in `docs/design/dx-<slug>.md` first; promote into published `docs/` (`zensical.toml`) **only** alongside the real working implementation.
2. **Problem-First Framing & Visual Hierarchy (`writer` + `review-ux`):**
   - Open user pages with clear user value (*"Slipfork runs your cloud environment locally in a box; CI is the clutch"*) and a working copy-pasteable 10-line CLI/`snow.yml` snippet in the first 25 lines — never lead with `$XDG_DATA_HOME`, `0600` file modes, Btrfs ioctls, or `"Mental model"` headers.
   - Include Mermaid workflow/`sequenceDiagram` diagrams, Zensical `!!! info` / `!!! warning` admonitions, narrow scannable tables, and `<= 2`-sentence paragraphs (`<= 280` chars).
   - Ban internal `ADR-*` / `M3.*` plan citations, bit-width lawyer tables, legacy dual syntaxes, and client names; verify with `uvx --offline zensical build --strict`.

## 2. High-Craft Terminal & CLI UX (`implementer` + `review-ux`)

- **Dynamic Column Widths:** Measure visible widths after stripping ANSI escapes (`.max()`) — never shear world/worktree IDs with fixed `{:<16}`/`{:<24}`.
- **Semantic ANSI Hierarchy & Purity:** Bold/cyan primary URLs, green `running`, red `failed`, dimmed 12-char digests; emit **zero** `\x1b[` escapes under `NO_COLOR`, non-TTY, or `--json`.
- **Live Progress on `> 200ms` Operations:** Use `spawn()` with `[step/total]` phase headers, spinners, and elapsed time — never freeze the terminal in silent `sleep` poll loops, `pull -q`, or blocking `.output()`.
- **Sharp Defaults & Remedies:** Format errors as `1 line + next:` remedy, put `doctor`'s `✓ Ready` / `✗ N issue(s)` verdict first, omit `:80` on port-80 URLs, warn on stderr when running ephemerally (`sudo slipfork daemon install`), and gate `[resources]` plumbing behind `-v`.
