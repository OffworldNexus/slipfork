---
id: M3
title: "Clean-Sheet Slipfork 0.1.x: Snapshot-Graph Btrfs Engine, ForkLedger, Image-Identity Pods, Report CLI, Docs & All-Distro Packaging"
kind: milestone
status: done
priority: 3
depends_on: [M2]
paths: []
attempts: 0
max_attempts: 3
escalation: strategist
created: "2026-11-14T22:03"
updated: "2026-11-26T07:50"
---

# Goal

Close Slipfork **`0.1.x`** (`0.1.0` published; **`0.1.1`** on `release/0.1.1`, draft PR #8) over the unified pure-Btrfs `SnapshotStore` + `ForkLedger` engine, `:80` `.localhost` ingress, single `Report` CLI, visual Zensical docs, and 18-artifact Linux/LSP distribution pipeline:

1. **Snapshot-graph Btrfs engine & `ForkLedger` (`specs/03` §1–§7):** `BuildGraph` + `SnapshotStore` (`reconcile`, `Lease`, `gc`, `--no-cache`), `StateDb` (`sea-orm` SQLite WAL `state.db`) + `ForkLedger` flat `<root>/.forks/<name>` with parent-splice `drop` and `ConvergeExecutor`, image-`USER` `crun` pods (default `0:0` mapped 1:1 to host uid, `0022` umask), native Rust S3 (`private` / `public-read` + cloud `cdn`), direct-exec PG18/Redis7, and `slipfork docker` (`login`/`logout`/`list`/`images`/`pull`/`build`).
2. **Package-managed `:80` `.localhost` ingress & per-user daemon (`specs/06`):** `.deb`/`.rpm`/pacman install hooks and `docs/install.sh` enable user `slipfork.service`, `slipfork-ingress.socket` + `slipfork-ingress@.service`, and `/etc/apparmor.d/slipfork` (`OpenRC` `slipfork-ingress` on Alpine); `slipfork auth` stays deferred to `M4` (`0.2.0`) while `slipfork-ospp` and local `0600` `secret()`/`var()` ship tested.
3. **Single `Report` CLI, `slipfork-lsp` & `@slipfork/opencode` (`specs/01`, `specs/02`, `specs/06`):** `# forkfile: 0.1` (`features`, `on`/`else`, `arg` with `service.port`, `dev` `WORKDIR` mounts, `fmt`, `migrate`), `Report`/`Row`/`Outcome`/`Failure` + `ReportRenderer` + `WallZone`, append-only live boot output, durable run catalog (`logs -r`, `--runs`, `--last N`), real-PTY `shell <target>` & `exec`, standalone 6-target `slipfork-lsp` + Zed/Vim, and `@slipfork/opencode`.

# Acceptance (`0.1.1` Release Gate Checklist)

- [x] Workspace `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `editors/zed/check.sh`, `tools/guard.py`, `tools/plan.py validate`, and 15-distro `packages.yml` build + install rehearsal pass.
- [ ] **PR #8 CI green (`M3.180`):** cached `image` reconciles on `up` emit one `✓ [i/n] image <id> cached <dur>` row with zero inner rows (`critical_world_journey` 5/5 green on live Btrfs), and `.gitattributes` enforces LF so `slipfork-forkfile` tests pass on `windows/{x86_64,aarch64}`.
- [ ] **All 16 live suites & isolated plugin gate (`M3.181`):** `Makefile` `LIVE_SUITES` includes `slipfork-world/critical_snapshot_graph` and `slipfork/logs_last_live`, `packages/opencode-slipfork/tests/e2e.test.ts` isolates `XDG_RUNTIME_DIR`/`XDG_STATE_HOME` + is guarded by `critical_guard.py`, and `Makefile`/spec version defaults match `0.1.1`.
- [ ] **Five Engineering Laws intact (`M3.182`):** zero production `.rs` files `> 500` LOC, `Palette` raw SGR (`tint`) closed behind named role methods, `boot.rs` prints routed through `ui::report`, `pty.rs` `static RESIZED` removed, and dead `auth` residue deleted.
- [ ] **Zero spec, `TESTING_PLAN`, `CHANGELOG`, or `docs/` drift (`M3.183` + owner `docs/` pass):** `specs/02`–`04`, `TESTING_PLAN.md`, `CHANGELOG.md` `[0.1.1]`, and `docs/` (`make docs-check`) match shipped append-only boot, image-`USER` pods, `ForkLedger`, `slipfork docker`, `up --no-cache`, and `logs --last`.
