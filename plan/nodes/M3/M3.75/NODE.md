---
id: M3.75
title: Retire slipfork auth from the 0.1.0 CLI and mark the release very experimental
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/surface.rs
  - "crates/slipfork/src/world/**"
  - crates/slipfork/src/compose.rs
  - "crates/slipfork/tests/**"
  - "packaging/man/**"
  - "docs/**"
  - "specs/05-ospp-v1.md"
  - README.md
  - CHANGELOG.md
  - .offworld/project.toml
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-23T22:36"
updated: "2026-11-23T22:42"
---

# Goal

`slipfork auth` (OSPP v1 pairing) is deferred past 0.1.0: **comment the command out** so it is not a shipped 0.1.0 surface, keep `crates/slipfork/src/commands/auth.rs` on disk dormant for a future release, and make every user-facing surface say this release is very experimental.

# Pre-baked findings

- CLI variant: `crates/slipfork/src/cli/verbs.rs` (`Auth { url, ospp_endpoint }`, ~line 263) and its dispatch arm in `crates/slipfork/src/cli/dispatch.rs` (~line 151) plus the `AuthArgs` import there.
- Wiring: `crates/slipfork/src/commands/mod.rs` (`mod auth;` line 18, `pub use auth::AuthArgs;` line 29). Comment both out; leave `commands/auth.rs` in the tree with a one-line module note that it is dormant until OSPP pairing ships (do not delete the OSPP crate or the daemon's saved-pairing path).
- Man page is generated: `cargo run -q -p slipfork -- --manpage > packaging/man/slipfork.1` (keep `tests/manpage_drift.rs` green).
- Protected tests that drive the CLI (authorized by this node): `crates/slipfork/tests/critical_cli_surface.rs` (the `auth` matrix entry ~139 and `("auth", &["--ospp-endpoint","--json"])` ~244), `crates/slipfork/tests/ansi_matrix.rs` (~102), and `crates/slipfork/tests/critical_host_verbs.rs` (module doc line 1 + `auth_pairs_through_the_loopback_reference_server` ~280 and `auth_without_an_endpoint_fails_with_one_next_line` ~336). Remove those tests/entries and any helper that becomes unused (clippy `-D warnings` must stay green); do not delete the files themselves.
- Published docs naming the command: `docs/cli.md` (~56), `docs/ospp.md` (pairing quickstart + several references), `docs/getting-started.md` (~57), `docs/troubleshooting.md` (~97-98). Published docs may not name a command that no longer exists (`tools/check_docs_honesty.py`). `specs/05-ospp-v1.md` is the target contract: keep the wire spec, but state once that the pairing CLI is deferred past 0.1.0.
- `CHANGELOG.md` still says `## [Unreleased] - 2026-11-22`; cut it to `## [0.1.0] - 2026-11-23` and add a `Removed` entry for `slipfork auth` that says it returns in a later release.

# Very experimental release

- `README.md`: one short blockquote after the tagline (<= 2 sentences) stating 0.1.0 is a **very experimental** release and pointing at the issue tracker.
- `docs/index.md`: one `!!! warning "Very experimental release"` admonition near the top (<= 2 sentences).
- `docs/install.md`: one sentence in the intro (same message, no repetition beyond that).
- `.offworld/project.toml`: `status = "experimental"`; keep slug/name/tagline/description/packages/formats unchanged. Validate with
  `uv run --no-project python /home/remy/offworld-nexus/.opencode/skills/offworld-packaging/scripts/validate_project.py .offworld/project.toml`
  and copy its normalised output back if it rewrites formatting.

# Acceptance

- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings`.
- `cargo test -p slipfork --test critical_cli_surface --test critical_host_verbs --test ansi_matrix --test manpage_drift` green.
- `make check` and `make docs-check` exit 0; `grep -rn "slipfork auth" docs README.md packaging/man .offworld` has no hits (specs may keep one explicitly-deferred mention).
- `validate_project.py` passes and prints `status = "experimental"`.
- `M3.75 -> done`, explicit-path commit `[M3.75] ...`.
