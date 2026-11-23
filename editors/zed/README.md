# Forkfile for Zed

This directory is a **dev extension** for Zed. It points at the in-repo grammar
source and at the standalone `slipfork-lsp` binary; nothing is vendored here.

```bash
# one-time: build the language server and put it on PATH
cargo install --path ../../crates/slipfork-lsp
# Zed: Extensions -> Install Dev Extension -> choose editors/zed
```

`slipfork-lsp` is a portable binary on Linux, macOS, and Windows, so the same
extension works on every developer laptop. The extension resolves the server in
exactly three steps:

1. `lsp.slipfork-lsp.binary.path` in your Zed settings (point it at any build).
2. `slipfork-lsp` on `PATH` (`cargo install --path crates/slipfork-lsp`).
3. One GitHub Release download for this platform, unpacked once into the
   extension's work directory and reused from disk afterwards.

The downloaded asset names are
`slipfork-lsp-{linux,darwin}-{x86_64,aarch64}.tar.gz` and
`slipfork-lsp-windows-{x86_64,aarch64}.zip`. If none applies, the error names
the matrix and the `lsp.slipfork-lsp.binary.path` setting.

The manifest uses `[grammars.forkfile] repository = "file://../tree-sitter-forkfile"`
with `rev = "HEAD"` so Zed builds the canonical grammar in
`editors/tree-sitter-forkfile` directly.

## Publishing

Zed's extension registry cannot fetch a `file://` grammar. Publishing requires
moving `editors/tree-sitter-forkfile` into its own public repository, then
changing `repository` to that HTTPS URL and pinning `rev` to a tag or commit.

`./check.sh` validates the manifest and fails if the Zed highlight query drifts
from the canonical grammar query.
