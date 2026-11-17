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

Publish exactly the surface `M3.28.13` verified: bare status, `restart`, `ps`, `open`, `fork` with no argument, and the `:80` ingress installed by `daemon install`. Zero vaporware, zero compatibility prose.

# Context

Published docs are the dream-tool contract; `tools/check_docs_honesty.py` builds the real binary and rejects any documented subcommand missing from `slipfork --help`. The current pages still describe the old world: the `$XDG_RUNTIME_DIR` runtime-dir default, "the ingress port is ephemeral, never assume `:80`", the three-step `daemon unit | sudo tee` install, and a `list`-only workflow.

# Design / Invariants

1. `docs/reference/cli.md`: document `status` (and that bare `slipfork` runs it), `restart`, `ps`, `open`/`open --remote`, `daemon install`, `-v/--verbose`, the port-less origin and the SSH-forward recipe. Delete the superseded runtime-dir default, the ephemeral-port warnings and the three-step unit install.
2. `docs/getting-started.md`: quickstart = install → `slipfork doctor` → `slipfork daemon install` (one root line) → `slipfork up` → `slipfork open`. No page over ~170 lines; lead with the working example.
3. `docs/index.md` + `README.md`: same surface in shortest form, port-less URL, one command per line; vision prose stays in `docs/VISION.md`.
4. `CHANGELOG.md`: one `### Added`/`### Changed` block under `0.1.0` naming the verbs and the `:80` ingress; no version bump (single version source is `M3.23`).
5. Delete, never dual-document: no old/new pairs, no strikethrough history, no "`status` used to be `--help`". Anything `M3.28.13` did not verify is removed or reported to the lead, never published.

# Acceptance

```console
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (N published doc(s) match real CLI subcommands)
$ uvx --offline zensical build --strict
... exit 0
$ for c in status restart ps open 'daemon install'; do grep -rqF "slipfork $c" docs/reference/cli.md || { echo "missing: $c"; exit 1; }; done; echo surface-documented
surface-documented
$ ! grep -rn 'XDG_RUNTIME_DIR/slipfork\|sudo tee\|never assume' docs/reference/cli.md docs/getting-started.md README.md && echo superseded-gone
superseded-gone
$ [ "$(wc -l < docs/getting-started.md)" -le 170 ] && echo lean
lean
$ make docs-check && make check
... exit 0
```

- `check_docs_honesty.py` exits 0 and the grep loop proves each shipped verb is documented while the inverted grep proves the superseded text is gone (both must hold).
