#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
version="0.27.2"

if command -v tree-sitter >/dev/null 2>&1; then
  cli="$(command -v tree-sitter)"
else
  cli="${XDG_CACHE_HOME:-$HOME/.cache}/slipfork/tree-sitter"
  if [ ! -x "$cli" ]; then
    case "$(uname -m)" in
      x86_64) asset="tree-sitter-linux-x64.gz" ;;
      aarch64) asset="tree-sitter-linux-arm64.gz" ;;
      *)
        echo "error: no prebuilt tree-sitter CLI for $(uname -m)"
        echo "next: install tree-sitter in PATH and rerun editors/tree-sitter-forkfile/test.sh"
        exit 1
        ;;
    esac
    mkdir -p "$(dirname "$cli")"
    url="https://github.com/tree-sitter/tree-sitter/releases/download/v${version}/${asset}"
    if ! curl -fsSL "$url" | gunzip > "$cli"; then
      rm -f "$cli"
      echo "error: cannot download the official tree-sitter CLI"
      echo "next: install tree-sitter in PATH and rerun editors/tree-sitter-forkfile/test.sh"
      exit 1
    fi
    chmod +x "$cli"
  fi
fi

cd "$here"
"$cli" test "$@"
