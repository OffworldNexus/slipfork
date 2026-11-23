# Forkfile for Vim & Neovim

Open a `Forkfile` and it just works: `# forkfile: 0.1` is detected, directives,
components, `@tool` runners, `$port`, `$env.KEY` and `"${...}"` interpolation
are highlighted, and `{ ... }` blocks indent 2 spaces — no plugins, no LSP required.

## Install

Vim (8+):

```sh
mkdir -p ~/.vim/pack/slipfork/start
cp -r editors/vim ~/.vim/pack/slipfork/start/forkfile
```

Neovim:

```sh
mkdir -p ~/.config/nvim/pack/slipfork/start
cp -r editors/vim ~/.config/nvim/pack/slipfork/start/forkfile
```

The pack directory is autoloaded once `filetype plugin indent on` and `syntax on`
are enabled (default in Neovim and in most Vim configs).

## What you get

```forkfile
# forkfile: 0.1
image api . {
  service web {
    port 8000
    route /api
    run: @uv run uvicorn app --port $port
  }
}
```

`# forkfile: 0.1` is `PreProc`; directives (`image`, `service`, `on`, `else`,
`env`, …) are `Keyword`; declared component ids are `Type`; dotted properties
(`cache.url`, `.password`) and `$env.KEY` references are `Identifier`; `@uv`/
`@pnpm`/`@cargo` are `Function`; `$port` is `Special`; `"${self.url}/login"`
highlights the builtin, operator, and dotted property inside the interpolation.

## Editing ergonomics

- 2-space indent, `expandtab`, `{`/`}` aware; `} else {` and
  `} else on dev {` align with the opening block and stay net-zero.
- `commentstring=# %s` and `comments=:#`, so `gcc` / `gc` comment toggling works.

## Language server (optional)

A separate `slipfork-lsp` server exists for diagnostics, hover, and
go-to-definition; this plugin never requires it and does not pretend to ship it.
Detection, syntax, indentation and comment toggling work on their own.

Run `editors/vim/check.sh` for headless assertions against `support/demo/Forkfile`.
