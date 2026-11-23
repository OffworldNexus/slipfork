---
id: M3.30.3
title: "High-craft terminal UX: TerminalUi palette, live streamed progress/spinners, dynamic column alignment"
kind: epic
status: done
priority: 3
depends_on: [M3.30.2]
paths: [Cargo.toml, Cargo.lock, "crates/**", "packages/opencode-slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T22:19"
updated: "2026-11-19T02:02"
---

# Goal
Make every Slipfork command feel like `cargo`/`gh`: semantic color that disappears cleanly when piped, live streamed progress on anything slower than `200ms`, and tables whose columns fit their real content.

# Context
Audit-verified anchors (HEAD). Do NOT re-research.

## 1. Palette & capability detection
- Add `owo-colors` + `indicatif`; add Clap `Styles` at `crates/slipfork/src/cli.rs:28`.
- Create one cohesive `TerminalUi` struct at `crates/slipfork/src/ui.rs` owning: `std::io::IsTerminal` detection, `NO_COLOR`, `--json`, `-v` verbosity, spinner handle, and all styling helpers.
- Contract: zero `\x1b[` bytes when piped, under `NO_COLOR=1`, or with `--json`. Bold cyan primary URLs (omit `:80`), green `running` / `✓`, yellow `!`, red `failed` / `✗`, dimmed 12-char digests and secondary paths on TTY only.

## 2. Live progress (`> 200ms` ⇒ spinner, never a silent wait)
- Add `BootEvent::Progress { step: u8, total: u8, phase: String, detail: Option<String> }` to the boot event enum in `crates/slipfork-edge/src/daemon.rs` and render it in `TerminalUi` with `[step/total]` headers + elapsed time.
- `crates/slipfork-engine/src/rootfs/buildx.rs:68-108`: replace blocking `.output()` around `docker buildx --progress=plain` with `spawn()` + line-streamed phase progress.
- `crates/slipfork-engine/src/rootfs/oci_store.rs:62-186`: same for `docker pull` / `docker export`; drop `-q` and `.output()`, stream live lines.
- `crates/slipfork/src/main.rs:1615` silent 15s daemon-wait `sleep` loop → `indicatif` spinner with elapsed time and a clear `next:` on timeout.
- Any other `> 200ms` subprocess/progress site encountered in these files gets the same treatment (`git` plumbing keeps pinned `GIT_AUTHOR_*`/`GIT_COMMITTER_*`).

## 3. Dynamic tables & hierarchy
Replace every hardcoded width with ANSI-stripped `.max()` visible-width computation:
- `crates/slipfork/src/commands/status.rs` (`LABEL_WIDTH` and `{:<}` sites) and `crates/slipfork/src/commands/ps.rs`.
- `list`, `forward`, `secret` renderers (hardcoded `{:<28}` / `{:<40}` / `{:<24}`).
- `slipfork doctor` (`crates/slipfork/src/doctor.rs`): bold top-line verdict `✓ Ready` or `✗ N issue(s)` followed by `next: sudo slipfork daemon install`; advisory checks (e.g. `noatime`) visually subordinated to fatal ones.
- Errors stay `1 line + next:` remedy format; `[resources]` jargon only under `-v`.
- `packages/opencode-slipfork/**`: keep plugin output free of ANSI when not a TTY.

# Deliverables
- `crates/slipfork/src/ui.rs` `TerminalUi` + unit tests for palette gating and width math (pure algorithm tests only).
- All listed render sites migrated; no `{:<28}`, `{:<40}`, `{:<24}`, `LABEL_WIDTH` literals remain.
- Live progress on rootfs build/pull/export and daemon-wait.

# Constraints
- Never swallow child output: stream, don't buffer.
- `--json` output stays machine-pure (no spinners, no ANSI, no phase headers on stdout — use stderr for progress).
- No new single-impl traits; `TerminalUi` is a concrete struct constructed once in the composition root.

# Acceptance
- `! grep -rn "{:<28}\|{:<40}\|{:<24}\|LABEL_WIDTH" crates/slipfork/src/commands/ crates/slipfork/src/doctor.rs`
- `! grep -rn "\-q\"\|\.output()" crates/slipfork-engine/src/rootfs/oci_store.rs crates/slipfork-engine/src/rootfs/buildx.rs`
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib ui::`
- `NO_COLOR=1 cargo run -q -p slipfork -- status 2>/dev/null | grep -c $'\x1b\[' || true` → must print `0`
- `cargo run -q -p slipfork -- --json status | head -1` → valid JSON
