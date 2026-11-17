---
id: M3.28.14
title: "Publish the 0.1.0 UX surface: CLI reference, getting started, index, README, CHANGELOG"
kind: task
status: ready
owner: "doc-writer"
priority: 2
depends_on: [M3.28.13]
paths: [docs/reference/cli.md, "docs/getting-started.md", docs/index.md, README.md, CHANGELOG.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:34"
updated: "2026-11-17T16:34"
---

# Goal

Publish exactly the surface `M3.28.13` verified: bare status, `restart`, `ps`, `open`, `fork` with no argument, the `lineage` line on `up`, and the `:80` ingress installed by `daemon install`. `dag` does not exist in `0.1.0` and no page may imply it will.



# Context

Published docs are the dream-tool contract; `tools/check_docs_honesty.py` builds the real binary and rejects any documented subcommand missing from `slipfork --help`. `M3.28.11` already deleted the `dag` subcommand, `docs/testing-dag.md`, its `zensical.toml` nav entry and the `AGENTS.md` claim — do not resurrect any of them, and do not add a `0.1.0` roadmap teaser for planning (that idea is parked on `M3.2` for `0.2.0`). The current pages still describe the rest of the old world: the `$XDG_RUNTIME_DIR` runtime-dir default, "the ingress port is ephemeral, never assume `:80`", the three-step `daemon unit | sudo tee` install, and a `list`-only workflow.

# Design / Invariants

1. `docs/reference/cli.md`: document `status` (and that bare `slipfork` runs it), `restart`, `ps`, `open`/`open --remote`, `daemon install`, `-v/--verbose`, the port-less origin, the `lineage` line and the SSH-forward recipe. Delete the superseded runtime-dir default, the ephemeral-port warnings and the three-step unit install.
2. `docs/getting-started.md`: quickstart = install → `slipfork doctor` → `slipfork daemon install` (one root line) → `slipfork up` → `slipfork open`. No page over ~170 lines; lead with the working example.
3. `docs/index.md` + `README.md`: same surface in shortest form, port-less URL, one command per line; vision prose stays in `docs/VISION.md`. Any `dag`/DAG-planner mention is already deleted by `M3.28.11` — do not re-add it in any tense.
4. `CHANGELOG.md`: one `### Added`/`### Changed`/`### Removed` block under `0.1.0` naming the verbs, the `lineage` line, the `:80` ingress, and the removals of the experimental `dag plan` command and the unimplemented `slipfork share` command; no version bump (single version source is `M3.23`).
5. Delete, never dual-document: no old/new pairs, no strikethrough history, no "`status` used to be `--help`", no "coming in `0.2.0`" teaser for a plan view or for tunnel sharing. Anything `M3.28.13` did not verify is removed or reported to the lead, never published.

# Acceptance

```console
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (N published doc(s) match real CLI subcommands)
$ uvx --offline zensical build --strict
... exit 0
$ for c in status restart ps open 'daemon install'; do grep -rqF "slipfork $c" docs/reference/cli.md || { echo "missing: $c"; exit 1; }; done && grep -qF 'lineage' docs/reference/cli.md && echo surface-documented
surface-documented
$ ! grep -rniE '\bdag\b|DAG planner' docs/index.md docs/getting-started.md docs/reference/cli.md README.md && echo no-dag
no-dag
$ ! grep -rniE 'slipfork share|tunnel shar' docs/index.md docs/getting-started.md docs/reference/cli.md README.md docs/snow-modes.md docs/reference/manifest.md && echo no-share
no-share
$ ! grep -rn 'XDG_RUNTIME_DIR/slipfork\|sudo tee\|never assume' docs/reference/cli.md docs/getting-started.md README.md && echo superseded-gone
superseded-gone
$ [ "$(wc -l < docs/getting-started.md)" -le 170 ] && echo lean
lean
$ make docs-check && make check
... exit 0
```

- `check_docs_honesty.py` exits 0 and the grep loop proves each shipped verb is documented while the inverted grep proves the superseded text is gone (both must hold).
