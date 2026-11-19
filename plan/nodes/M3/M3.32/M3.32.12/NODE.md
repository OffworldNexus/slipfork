---
id: M3.32.12
title: "Forkfile syntax highlighting in Zensical docs (forkfile-highlight.js + WCAG AA CSS + JS tests)"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.32.9]
paths: ["docs/**", "packages/**", "tools/tests/**", zensical.toml, Makefile]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T16:03"
updated: "2026-11-19T16:03"
---

# Goal

Make every published `Forkfile` snippet genuinely highlighted, with light/dark accessible colors and tests.

# Context

OWNER ADDITION to `M3.32`. Pygments has no `forkfile` lexer, so ` ```text title="Forkfile" ` renders as `language-text`.

- Write all published `Forkfile` fences as ` ```text title="Forkfile" `.
- `docs/javascripts/forkfile-highlight.js`: `"use strict"` IIFE, zero modules/eval/network, wired via `zensical.toml` `extra_javascript` + `pymdownx.highlight.pygments_lang_class = true`. Classify `Forkfile` blocks only (ignore `error:`/`-->` diagnostic blocks), tokenize keywords (`features`,`on`,`postgres`,`database`,`redis`,`s3`,`domain`,`env`,`use`,`service`,`from`,`stage`,`port`,`route`,`volume`,`setup`,`pre-deploy`,`post-deploy`,`failed-deploy`,`run`,`task`,`in`), builtins (`secret`,`var`,`random_hex`,`random_base64`,`random_string`,`uuid4`), `$var` interpolations, `<id>.<prop>` refs, strings, numbers, operators (`??`,`+`,`&`,`|`,`!`), `#` comments. Walk DOM Text nodes ONLY (preserve line numbers and copy-button `textContent`), mark with `data-forkfile-highlighted`.
- `docs/stylesheets/extra.css`: light + `[data-md-color-scheme="slate"]` token colors at WCAG AA >= 4.5:1.
- Tests (Bun/Node under `tools/tests/`): every published `Forkfile` fence highlights, diagnostics stay unhighlighted, markup/copy text preserved, contrast passes.

# Acceptance

- `uvx --offline zensical build --strict` and `make docs-check` exit 0; the new JS test suite passes and is wired into `make check`.
- Write NO `RESULT.md`.
