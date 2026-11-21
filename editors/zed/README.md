# Forkfile for Zed

This directory is a **dev extension** for Zed. It points at the in-repo grammar
source and at the shipped `slipfork lsp` binary; nothing is vendored here.

```bash
# one-time: install the CLI and put it on PATH
cargo install --path ../../crates/slipfork
# Zed: Extensions -> Install Dev Extension -> choose editors/zed
```

The manifest uses `[grammars.forkfile] repository = "file://../tree-sitter-forkfile"`
with `rev = "HEAD"` so Zed builds the canonical grammar in
`editors/tree-sitter-forkfile` directly.

## Publishing

Zed's extension registry cannot fetch a `file://` grammar. Publishing requires
moving `editors/tree-sitter-forkfile` into its own public repository, then
changing `repository` to that HTTPS URL and pinning `rev` to a tag or commit.

`./check.sh` validates the manifest and fails if the Zed highlight query drifts
from the canonical grammar query.
