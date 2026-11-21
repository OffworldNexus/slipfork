#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

for key in \
  'id = "forkfile"' \
  'languages = ["languages/forkfile"]' \
  '[grammars.forkfile]' \
  'repository = "file://../tree-sitter-forkfile"' \
  '[language_servers.slipfork]'
do
  if ! grep -Fq "$key" "$here/extension.toml"; then
    echo "error: extension.toml is missing: $key"
    echo "next: restore the key from editors/zed/README.md"
    exit 1
  fi
done

if ! diff -u \
  "$here/../tree-sitter-forkfile/queries/highlights.scm" \
  "$here/languages/forkfile/highlights.scm"
then
  echo "next: copy editors/tree-sitter-forkfile/queries/highlights.scm over languages/forkfile/highlights.scm"
  exit 1
fi

echo "✓ zed extension manifest and highlights match the canonical grammar"
