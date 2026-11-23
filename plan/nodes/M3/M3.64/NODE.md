---
id: M3.64
title: "Sync LSP, Tree-sitter, Zed, Vim, distro packages, support/demo & docs with on/else + self.url + \"${...}\" + secret/var(KEY, gen) & deploy to VM"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.63]
paths:
  - "crates/slipfork-lsp/**"
  - "crates/slipfork/tests/**"
  - "editors/**"
  - "support/demo/**"
  - "specs/**"
  - "docs/**"
  - "scripts/**"
  - "packaging/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T01:06"
updated: "2026-11-23T01:11"
---

# Goal

Propagate all four `# forkfile: 0.1` language upgrades from `M3.63` (`on <expr> ... else ...`, `self.url` / `self.domain`, `"${...}"` string interpolation, and `secret(KEY, <generator>)` / `var(KEY, <generator>)` with `random_string(n)`) across `slipfork-lsp`, Tree-sitter, Zed, Vim, Linux packages, `support/demo/Forkfile`, `specs/`, and `docs/`, then deploy the updated `.deb`, Vim plugin, and `~/demo/Forkfile` onto `165.232.86.174`.

# Scope

1. **`crates/slipfork-lsp`:**
   - Update completions, hover docs, and diagnostics for `else`, `self` (`self.url`, `self.domain`), `"${...}"` interpolation, `secret(KEY, <generator>)`, `var(KEY, <generator>)`, and `random_string(n)` (removing `random_alnum` and `${{ ... }}` remnants).
2. **`editors/tree-sitter-forkfile`, `editors/zed`, `editors/vim`:**
   - Update `grammar.js` + regenerated `src/parser.c` + `queries/highlights.scm` + corpus tests in `editors/tree-sitter-forkfile` for `else` (`else { ... }`, `else: ...`, `else on ...`), `self.url`/`self.domain`, `"${...}"` string interpolation, `secret(KEY, gen)` / `var(KEY, gen)`, and `random_string` (scorched-earth removing `random_alnum` and `${{ ... }}`).
   - Sync `editors/zed/languages/forkfile/highlights.scm` and `editors/vim/{syntax,indent}/forkfile.vim` + `editors/vim/check.sh`.
3. **Bundle Vim plugin in Linux packages (`scripts/` & `packaging/`):**
   - Stage `editors/vim/{ftdetect,syntax,indent,ftplugin}/forkfile.vim` into `share/slipfork/vim/` in `scripts/build-bundle.sh`, verify in `scripts/check-bundle-layout.sh` + `scripts/check-install.sh`, and install into `/usr/share/vim/vimfiles/{ftdetect,syntax,indent,ftplugin}/forkfile.vim` across `scripts/build-deb.sh`, `packaging/rpm/slipfork.spec`, `packaging/arch/PKGBUILD`, and `packaging/alpine/APKBUILD`.
4. **`support/demo/Forkfile`, `specs/`, and `docs/`:**
   - Update `support/demo/Forkfile`:
     - Delete bogus `domain main`!
     - Use `PUBLIC_URL = self.url`, showcase `"${self.url}/..."` string construction (`LOGIN_URL = "${self.url}/login"`), `SECRET_KEY = secret(SECRET_KEY, random_string(50))`, `THIRD_PARTY_API_URL = var(THIRD_PARTY_API_URL) ?? "https://example.invalid"`, and `on dev { ... } else { ... }` (eliminating `on !dev` mirror blocks!).
   - Update `specs/01-forkfile.md`, `specs/02-cli-ux.md` / `specs/03-under-the-hood.md` (any snippet mentioning `random_alnum`, `?? random_*`, or `domain main`), and `docs/` (`zensical --strict` + `tools/check_docs_honesty.py`).
5. **Live Droplet Redeploy & Live Suites (`165.232.86.174`):**
   - Build the portable `glibc <= 2.34` `.deb`, install on `165.232.86.174` (which installs `/usr/share/vim/vimfiles/...` automatically too, plus sync `/home/slipfork/.vim/pack/slipfork/start/forkfile`!), run `critical_world_journey`, `critical_fork_live`, and `critical_status_live` under `SLIPFORK_TEST_ROOT=/var/lib/slipfork/t`, then sync `/home/slipfork/demo` and verify `demo` + `feat-auth` running (`200` on `:80`) + `youpi` stopped!

# Acceptance

- `make check`, `make docs-check`, `editors/tree-sitter-forkfile/test.sh`, `editors/zed/check.sh`, and `editors/vim/check.sh` exit `0`.
- All 3 live CLI suites (`critical_world_journey`, `critical_fork_live`, `critical_status_live`) pass on `165.232.86.174`, and `/home/slipfork/demo/Forkfile` runs live on `:80` with the new syntax.
