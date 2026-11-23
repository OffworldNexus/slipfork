---
id: M3.61
title: "Add Vim/Neovim Forkfile plugin (editors/vim) & install on 165.232.86.174"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["editors/vim/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T00:54"
updated: "2026-11-23T01:02"
---

# Goal

Author a clean, dependency-free Vim 8+ / Neovim runtime plugin for `Forkfile` in `editors/vim/` and install it live onto `165.232.86.174` so `vim Forkfile` highlights and indents `# forkfile: 0.1` out of the box.

# Scope (`editors/vim/**`)

1. **`editors/vim/ftdetect/forkfile.vim`:**
   - Detect `Forkfile`, `*.forkfile`, and Line 1 `# forkfile:` (`setfiletype forkfile`).
2. **`editors/vim/syntax/forkfile.vim`:**
   - Match the exact `# forkfile: 0.1` grammar (`editors/tree-sitter-forkfile/queries/highlights.scm` & `specs/01-forkfile.md`):
     - Header (`^#\s*forkfile:\s*\S\+` -> `PreProc`) and `# ...` comments (`Comment`, including `TODO`/`FIXME`/`NOTE`).
     - Block/directive keywords (`features`, `default`, `image`, `from`, `dockerfile`, `stage`, `workdir`, `arg`, `use`, `env`, `before-start`, `pre-deploy`, `post-deploy`, `failed-deploy`, `run`, `when`, `changed`, `service`, `port`, `internal_ports`, `route`, `strip-prefix`, `rewrite`, `exact`, `domain`, `postgres`, `database`, `version`, `redis`, `s3`, `cdn`, `policy`, `on` -> `Keyword`).
     - Declared component names after `image`, `service`, `postgres`, `database`, `redis`, `s3`, `cdn`, `domain` -> `Type`.
     - `@<tool>` prefixes (`@uv`, `@pnpm`, `@npm`, `@yarn`, `@bun`, `@cargo`, `@go`, `@poetry`, `@pip`, `@bundle`, `@composer`, `@mix`) -> `Function`.
     - `${{ ... }}` interpolation expressions with built-ins (`var`, `secret`, `random_hex`, `random_base64`, `random_alnum`, `random_uuid` -> `Function`), `<id>.<prop>` (`url`, `host`, `port`, `domain`, `endpoint`, `bucket`, `region`, `access_key_id`, `secret_access_key`), and operators (`??`, `+`, `|`, `&`, `!`, `=`, `:`).
     - `$port` / `$VAR` references (`Identifier` / `Special`), strings (`"..."`, `'...'` with escapes & `${{ ... }}` contained), booleans (`true`, `false`), integers, and route paths (`/api`, `/` -> `Special`).
3. **`editors/vim/indent/forkfile.vim` & `editors/vim/ftplugin/forkfile.vim`:**
   - Clean 2-space `{ ... }` block indentation (`shiftwidth=2 softtabstop=2 expandtab`), `commentstring=#\ %s`, `b:undo_ftplugin` / `b:undo_indent`.
4. **`editors/vim/check.sh` & `editors/vim/README.md`:**
   - Executable `check.sh` verifying syntax & filetype detection headless against `support/demo/Forkfile` (using `vim` locally if installed, or on `165.232.86.174`), plus concise install one-liners for Vim (`~/.vim/pack/slipfork/start/forkfile`) and Neovim (`~/.config/nvim/pack/slipfork/start/forkfile`) + `slipfork-lsp` wiring snippet.
5. **Live VM installation (`165.232.86.174`):**
   - Install into `/usr/share/vim/vimfiles/{ftdetect,syntax,indent,ftplugin}/forkfile.vim` AND `/home/slipfork/.vim/pack/slipfork/start/forkfile` (owned by `slipfork:slipfork`) + `/root/.vim/pack/slipfork/start/forkfile`, and ensure `/etc/vim/vimrc.local` enables `syntax on` and `filetype plugin indent on` so `vim ~/demo/Forkfile` highlights immediately even with zero `~/.vimrc`!

# Acceptance

- `editors/vim/check.sh` exits `0`.
- Headless `vim` probe on `165.232.86.174` opening `/home/slipfork/demo/Forkfile` as user `slipfork` reports `filetype=forkfile`, `syntax=forkfile`, and highlights `image` / `@uv` / `${{ ... }}` with real syntax groups.
