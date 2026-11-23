---
id: M3.66
title: "Sync LSP, Tree-sitter, Zed, Vim, distro packages, support/demo & specs/docs with locked Forkfile 0.1 contract & deploy to VM"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.65]
paths:
  - "crates/slipfork-lsp/**"
  - "crates/slipfork/tests/**"
  - "editors/**"
  - "support/demo/**"
  - "specs/**"
  - "docs/**"
  - "scripts/**"
  - "packaging/**"
  - README.md
  - TESTING_PLAN.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T01:11"
updated: "2026-11-23T01:21"
---

# Goal

Clear every trace of the old `<domain>.url` / `<domain>.domain` / `?? random_*` / `random_alnum` / `on !dev` mirror patterns from `slipfork-lsp`, Tree-sitter, Zed, Vim, Linux packages, `support/demo/Forkfile`, `specs/`, and `docs/`, then verify all live suites and redeploy `165.232.86.174`.

# Scope

1. **`crates/slipfork-lsp`:**
   - Complete & document `else`, `self` (`self.url`, `self.domain`, `self.scheme`), `<domain>.name` (zero `.url` / `.domain` on `domain` declarations!), `"${...}"` string interpolation, `secret(KEY, <generator>)`, `var(KEY, <generator>)`, and `random_string(n)` (zero `random_alnum`).
2. **`editors/tree-sitter-forkfile`, `editors/zed`, `editors/vim`:**
   - Update `grammar.js` + regenerated `src/parser.c` + `queries/highlights.scm` + corpus tests in `editors/tree-sitter-forkfile` for `on ... else ...` (`else { ... }`, `else: ...`, `else on ...`), `self.{url,domain,scheme}`, `<domain>.name`, `"${...}"` interpolation inside double-quoted strings (deleting `${{ ... }}`), `secret(KEY, gen)` / `var(KEY, gen)`, and `random_string` (deleting `random_alnum`).
   - Sync `editors/zed/languages/forkfile/highlights.scm` and `editors/vim/{syntax,indent}/forkfile.vim` + `editors/vim/check.sh`.
3. **Bundle Vim plugin in Linux packages (`scripts/` & `packaging/`):**
   - Stage `editors/vim/{ftdetect,syntax,indent,ftplugin}/forkfile.vim` into `share/slipfork/vim/` in `scripts/build-bundle.sh`, verify in `scripts/check-bundle-layout.sh` + `scripts/check-install.sh`, and install into `/usr/share/vim/vimfiles/{ftdetect,syntax,indent,ftplugin}/forkfile.vim` across `scripts/build-deb.sh`, `packaging/rpm/slipfork.spec`, `packaging/arch/PKGBUILD`, and `packaging/alpine/APKBUILD`.
4. **`support/demo/Forkfile`, `specs/`, `docs/`, and live `crates/slipfork/tests/critical_*.rs`:**
   - Update `support/demo/Forkfile` (KEEP `domain main`!):
     - `PUBLIC_URL = self.url`
     - `LOGIN_URL = "${self.url}/login"` (or similar `"${self.url}/..."` string construction)
     - `COOKIE_DOMAIN = main.name`
     - `SECRET_KEY = secret(SECRET_KEY, random_string(50))`
     - `THIRD_PARTY_API_URL = var(THIRD_PARTY_API_URL) ?? "https://thirdparty.com"`
     - Replace `on !dev` + `on dev` mirror pairs with `on dev { ... } else { ... }`!
   - Purge EVERY `<domain>.url`, `<domain>.domain`, `?? random_*`, `random_alnum`, and `${{ ... }}` misconception from `specs/01-forkfile.md`..`specs/06-distribution-and-docs.md` and `docs/**` — explicitly teaching the sharp `domain` (`<id>.name`, bare hostname without scheme) vs URL (`self.url`, `"${self.scheme}://${dom_b.name}"`, with scheme) distinction, `secret/var(KEY, <generator>)` vs deterministic `??`, and `on <expr> { ... } else { ... }`!
   - Update any fixture Forkfiles in `crates/slipfork/tests/critical_*.rs` (`main.url` -> `self.url` / `main.name`, `?? random_*` -> `secret(..., random_*(...))`).
5. **Live VM Verification & Redeploy (`165.232.86.174`):**
   - Run `critical_world_journey`, `critical_fork_live`, and `critical_status_live` on `165.232.86.174` (`SLIPFORK_TEST_ROOT=/var/lib/slipfork/t` isolated from `~/demo`).
   - Build the portable `glibc <= 2.34` `.deb`, install on `165.232.86.174` (updating `/usr/share/vim/vimfiles/...` + `/home/slipfork/.vim/pack/slipfork/start/forkfile`), sync `/home/slipfork/demo`, and verify `demo` + `feat-auth` running (`200` on `:80`) + `youpi` stopped!

# Acceptance

- `make check`, `make docs-check`, `editors/tree-sitter-forkfile/test.sh`, `editors/zed/check.sh`, and `editors/vim/check.sh` exit `0`.
- All 3 live CLI suites pass `100%` on `165.232.86.174`, and `/home/slipfork/demo/Forkfile` runs live on `:80`.
