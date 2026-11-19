# Forkfile for Zed

Syntax highlighting, outline, formatting, and diagnostics for `Forkfile` — the
single-file environment manifest that `slipfork` runs.

## Install

1. Install the CLI so `slipfork` is on your `PATH` (the extension runs the
   shipped `slipfork lsp`; it never bundles its own server):

   ```bash
   cargo install --path crates/slipfork
   ```

2. Install the **Forkfile** extension from Zed's extension registry, or load
   `editors/zed` as a dev extension (`zed: extensions` → *Install Dev Extension*).
3. Open any `Forkfile`.

## Settings

Paste this into `~/.config/zed/settings.json` (or a project `.zed/settings.json`)
to format on save and pin the language server:

```json
{
  "languages": {
    "Forkfile": {
      "format_on_save": "on",
      "language_servers": ["slipfork-lsp"]
    }
  }
}
```

`slipfork lsp` serves completion, hover, diagnostics, and document formatting
from the same parser and formatter the CLI uses, so editor feedback matches
`slipfork check` exactly.

## What you get

| Feature | Behaviour |
|---|---|
| Highlighting | Keywords, resource/database names, env keys, hooks, references |
| Outline | Projects, features, services, tasks, resources, env sections |
| Indentation | Braces and brackets from the tree-sitter grammar |
| LSP | Hover, completion, formatting, and diagnostics from `slipfork lsp` |

## Troubleshooting

- **"`slipfork` is not on PATH"** — install the CLI, then reload Zed
  (`zed: reload extensions`).
- **No highlighting** — confirm the file is named `Forkfile` or `forkfile`.
- **Stale tree shape** — the grammar under `editors/tree-sitter-forkfile` is
  generated from `grammar.js`; run `make grammar-test` (needs network for the
  tree-sitter CLI) to compare the parser against its corpus.
