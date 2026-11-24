---
name: DX-First (High-Craft CLI & Visual User Documentation)
description: MANDATORY for planner, writer, implementer, architect, review-ux, lead, and strategist. Everything stems from Developer Experience: problem-first specs/ and visual docs, rich ANSI/spinner CLI ergonomics, and zero published vaporware.
---

# DX-First: High-Craft CLI & Visual Documentation

Slipfork exists to make spinning up polyglot dev environments, forking agent worktrees, and running isolated stacks feel **instantaneous and invisible**. Every extra CLI flag, sheared table column, silent 10-second freeze, manual `.env` dance, or wall of lawyer prose is a defect.

## 1. Specs Before Code — Zero Published Vaporware

1. **Draft Target UX in `specs/` First:**
   - `specs/01-forkfile.md`, `specs/02-cli-ux.md`, and `specs/03-under-the-hood.md` are the contract. Never publish CLI commands, flags, or integrations in `README.md` or `docs/` that do not exist and run in the compiled `slipfork` binary (`uv run --no-project tools/check_docs_honesty.py` enforces this mechanically once `docs/` exists).
   - Promote target UX into published `README.md` / `docs/` **only** alongside the real working implementation.
2. **Problem-First Framing & Visual Hierarchy (`writer` + `review-ux`):**
   - Open user pages with clear user value (*"Slipfork runs your cloud environment locally in a box; CI is the clutch"*) and a working copy-pasteable `Forkfile` + CLI snippet in the first 25 lines — never lead with `$XDG_DATA_HOME`, `0600` file modes, Btrfs ioctls, or `"Mental model"` headers.
   - Include Mermaid workflow/`sequenceDiagram` diagrams, Zensical `!!! info` / `!!! warning` admonitions, narrow scannable tables, and `<= 2`-sentence paragraphs (`<= 280` chars).
   - Ban internal `ADR-*` / `M3.*` plan citations, bit-width lawyer tables, legacy dual syntaxes, and client names.
   - Packaging docs must show the same copy-pasteable verify command for `.deb`, `.rpm`, pacman, and APK installs, with systemd unit enablement.

## 2. High-Craft Terminal & CLI UX (`implementer` + `review-ux`)

- **Dynamic Column Widths:** Measure visible widths after stripping ANSI escapes (`.max()`) — never shear world/worktree IDs with fixed `{:<16}`/`{:<24}`.
- **Semantic ANSI Hierarchy & Purity:** Bold/cyan primary URLs, green `running`, red `failed`, dimmed 12-char digests; emit **zero** `\x1b[` escapes under `NO_COLOR`, non-TTY, or `--json`.
- **Live Progress on `> 200ms` Operations:** Use `spawn()` with `[step/total]` phase headers, spinners, and elapsed time — never freeze the terminal in silent `sleep` poll loops, `pull -q`, or blocking `.output()`.
- **Sharp Defaults & Remedies:** Format errors as `1 line + next:` remedy, put `doctor`'s `✓ Ready` / `✗ N issue(s)` verdict first, omit `:80` on port-80 URLs, warn on stderr when running ephemerally (the package enables the user service and the system `:80` socket for `SUDO_USER`; headless installs run `docs/install.sh`), and gate `[resources]` plumbing behind `-v`.
