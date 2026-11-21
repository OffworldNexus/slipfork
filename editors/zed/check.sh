#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

fail() {
  echo "error: $1"
  echo "next: $2"
  exit 1
}

for key in \
  'id = "forkfile"' \
  'languages = ["languages/forkfile"]' \
  '[grammars.forkfile]' \
  'repository = "file://../tree-sitter-forkfile"' \
  '[language_servers.slipfork-lsp]'
do
  grep -Fq "$key" "$here/extension.toml" ||
    fail "extension.toml is missing: $key" "restore the key from editors/zed/README.md"
done

grep -Fq 'language_servers = ["slipfork-lsp"]' "$here/languages/forkfile/config.toml" ||
  fail "languages/forkfile/config.toml does not launch slipfork-lsp" \
    'set language_servers = ["slipfork-lsp"]'

cargo check --manifest-path "$here/Cargo.toml" ||
  fail "the Zed extension does not compile" \
    "run cargo check --manifest-path editors/zed/Cargo.toml"

cargo test --manifest-path "$here/Cargo.toml" ||
  fail "the Zed extension tests fail" \
    "run cargo test --manifest-path editors/zed/Cargo.toml"

diff -u \
  "$here/../tree-sitter-forkfile/queries/highlights.scm" \
  "$here/languages/forkfile/highlights.scm" ||
  fail "languages/forkfile/highlights.scm drifted from the canonical grammar" \
    "copy editors/tree-sitter-forkfile/queries/highlights.scm over languages/forkfile/highlights.scm"

echo "✓ zed extension manifest, slipfork-lsp wiring, Rust build, and highlights all match"
