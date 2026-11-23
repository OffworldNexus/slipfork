---
id: M3.62
title: "Support on <expr> ... else ... (else { ... }, else: <item>, else on <expr>) across Forkfile compiler, fmt, LSP, editors, packages, demo & docs"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.60, M3.61]
paths:
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-lsp/**"
  - "crates/slipfork/**"
  - "editors/**"
  - "support/demo/**"
  - "specs/**"
  - "docs/**"
  - "scripts/**"
  - "packaging/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T00:54"
updated: "2026-11-23T01:06"
---

# Goal

Eliminate repetitive negated `on !dev` mirror blocks by supporting `else` (`else { ... }`, `else: <item>`, and `else on <expr> ...`) on `on <expr>` conditionals everywhere `on` is valid in `# forkfile: 0.1`.

# Design & Scope

1. **`crates/slipfork-forkfile` (Parser, AST, Evaluator, Formatter):**
   - Allow an optional `else` clause on `on <expr>` in all scopes where `on` is valid (top-level items, `image` items, `service` items, `s3` items, and `{ ... }` command blocks):
     - Block form: `on dev { ... } else { ... }`
     - Chained `else on <expr>` form: `on preview { ... } else on dev { ... } else { ... }`
     - Single-item `: ` form (both inline after `}` like `on dev { ... } else: stage prod` and line-following `on dev: stage dev` + `else: stage prod` / `else { ... }`).
   - Evaluation: if `<expr>` evaluates to `true`, evaluate the `then` branch; otherwise evaluate the `else` branch (if present). Validation, secret-taint analysis, and declared-domain / port / command scanning inspect both branches identically to `on`.
   - `slipfork fmt`: canonically format `} else {` and `} else on <expr> {` on the closing-brace line, and `else: <item>` cleanly.
2. **LSP, Tree-sitter, Zed, Vim & Linux Packages:**
   - Add `"else"` keyword + grammar rule in `crates/slipfork-lsp`, `editors/tree-sitter-forkfile` (`grammar.js` + regenerated `src/parser.c` + corpus test + `queries/highlights.scm`), `editors/zed/languages/forkfile/highlights.scm`, and `editors/vim`.
   - Ship `editors/vim/{ftdetect,syntax,indent,ftplugin}/forkfile.vim` inside `scripts/build-bundle.sh` (`share/slipfork/vim/...` -> `/usr/share/vim/vimfiles/...` in `.deb`, `.rpm`, `PKGBUILD`, `APKBUILD`, `check-bundle-layout.sh`, `check-install.sh`) so every distro package installs Vim `Forkfile` support automatically!
3. **Demo (`support/demo/Forkfile`), `specs/01-forkfile.md`, and `docs/`:**
   - Update `support/demo/Forkfile` to use `on dev { ... } else { ... }` instead of `on !dev` + `on dev` mirror pairs!
   - Document `on <expr> { ... } else { ... }` (and `else on`) in `specs/01-forkfile.md` and `docs/forkfile/features-and-overrides.md` (plus mention `editors/vim` in `docs/editors.md` or editor docs).
   - Rebuild & install the updated `.deb` on `165.232.86.174` and sync `/home/slipfork/demo/Forkfile` (`demo` + `feat-auth` running, `youpi` stopped).

# Acceptance

- `make check`, `make docs-check`, `editors/tree-sitter-forkfile/test.sh`, `editors/zed/check.sh`, and `editors/vim/check.sh` all exit `0`.
- Live droplet `165.232.86.174` runs the updated `.deb` and `/home/slipfork/demo/Forkfile` using `on dev { ... } else { ... }`.
