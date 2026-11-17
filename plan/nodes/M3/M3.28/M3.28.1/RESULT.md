# M3.28.1 RESULT — `0.1.0` UX DX Blueprint, `ARCHITECTURE.md` Seams, and `ADR-0011`

- **Status:** `done`
- **Node:** `M3.28.1` (`release/0.1.0`, base `1d4c08e`)
- **Deliverables:**
  - `docs/design/dx-0.1.0-ux.md` (new): canonical `0.1.0` UX specification, transcripts, fail-closed remedies, and `--json` contracts covering all 12 owner findings and all 7 locked decisions.
  - `docs/decisions/ADR-0011-port-80-daemon-install-and-runtime-dir.md` (new): records `slipfork daemon install`, system-level `:80` socket activation (`Requires=slipforkd.socket`, `After=slipforkd.socket`, `WantedBy=multi-user.target`), `:0 -> :80` handover (`DaemonControl::shutdown`), `<state_dir>/run` single runtime directory, and `doctor` `IngressProbe`.
  - `ARCHITECTURE.md`: updated §1 (Composition Root + Mermaid DI diagram), §2 (crate boundary table), and §§3.2–3.5 (`BrowserOpener::open_origin`, `IngressProbe`, `UnitInstaller`, `UserIdentity::lookup_account`, `HostIdentity`, `WorldEngine::world_processes`, `DaemonStatus.ingress_addrs`, `WorldUpSummary.{changed, restarted, worktree}`, `WorldServiceSummary`, `DaemonControl::{restart, world_processes, shutdown}`, `ForkRequest.fork: Option<String>`, `VcsWorkspaceFactory::current_branch`, `sanitize_fork_name`, `-c user.name=slipfork -c user.email=slipfork@localhost`).
  - `docs/design/target-dx.md`: amended §§1.1–1.7 in place so one unified CLI table and `--json` contract exist with zero superseded `daemon unit | sudo tee` or `$XDG_RUNTIME_DIR/slipfork` text.

## Verified Codebase Fact Checks (`1d4c08e`)

1. **`WorldOrigin::local(&route, ingress_port)` (`crates/slipfork-world/src/lib.rs:298-312`):** Verified — renders `http://<host>` at `80` and `http://<host>:<ingress_port>` otherwise.
2. **`LocalHostRoute::parse` + `WorldRegistry::resolve` (`crates/slipfork-world/src/lib.rs:213-243, 582-594`, `crates/slipfork-edge/src/registry.rs:487`):** Verified — `normalize_host` strips any numeric `:<port>`, so `ssh -N -L 8080:127.0.0.1:<ingress> <account>@<host>` with `Host: <route>.localhost:8080` routes to the identical world.
3. **`UnitSpec` (`crates/slipfork-edge/src/unit.rs:121-179`):** Verified — emits `ListenStream=127.0.0.1:80`, `[::1]:80`, `127.0.0.1:<forward_port>`. Caught two lifecycle gaps locked into `ADR-0011` §1: (a) `render_service_unit` needs `Requires=slipforkd.socket`, `After=slipforkd.socket`, and `WantedBy=multi-user.target` so `<state_dir>/run/daemon.sock` is served after reboot before the first CLI command, and (b) `daemon install` must call `DaemonControl::shutdown` when a non-`:80` auto-spawned daemon holds `daemon.sock`/`http-ingress.sock` (`EdgeError::SocketInUse` in `unix_socket.rs:76`).
4. **`DaemonStatus` (`crates/slipfork-edge/src/daemon.rs:43-102`):** Verified — holds `pid`, `http_ingress_port`, `forward_port`, `worlds`; extended with `ingress_addrs: Vec<SocketAddr>` and `WorldSummary.processes`.
5. **`BrowserOpener` (`crates/slipfork-ospp/src/pairing.rs:301`):** Verified — currently exposes `open_pairing_url(&self, url: LivePairingUrl)` where `LivePairingUrl` is private to `#handle=` fragments. Extended the same trait with `open_origin(&self, origin: &WorldOrigin) -> Result<(), OsppError>` (`slipfork-ospp` already depends on `slipfork-world`) and wired `SystemBrowserOpener` at `main.rs` (interactive desktop session) as well as `slipforkd.rs`.
6. **`UserIdentity` (`crates/slipfork/src/user.rs`):** Verified — `id -un` with `env_clear` + fixed `PATH`; extended in-place with `lookup_account` (refusing `uid == 0` / `root` and `%`) and `HostIdentity` for `open --remote`.

## Verified `# Acceptance` Output

```console
$ test -s docs/design/dx-0.1.0-ux.md && echo present
present
$ for t in 'slipfork status' 'slipfork restart' 'slipfork ps' 'slipfork open --remote' 'user.name=slipfork' 'slipfork daemon install' '<state_dir>/run' '127.0.0.1:80'; do grep -qF "$t" docs/design/dx-0.1.0-ux.md || { echo "missing: $t"; exit 1; }; done; echo tokens-ok
tokens-ok
$ test -s docs/decisions/ADR-0011-*.md && grep -qi 'composition root' ARCHITECTURE.md && echo arch-ok
arch-ok
$ ! grep -qE '\[(derived|guessed)\]|TODO|TBD' docs/design/dx-0.1.0-ux.md ARCHITECTURE.md docs/decisions/ADR-0011-*.md && echo no-placeholders
no-placeholders
$ uv run --no-project tools/guard.py
guard: OK (533 file(s) checked)
$ uv run --no-project tools/plan.py validate
plan OK
$ uv run --no-project tools/critical_guard.py --base 1d4c08e --node M3.28.1
critical-guard: 1d4c08e..working tree, 9 changed file(s)
  no change to a protected critical test
$ make docs-check
uvx --offline zensical build --strict
Build started
No issues found
Build finished in 0.19s
uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)
```

## Downstream Node Alignment Notes for `lead` (`plan.py edit`)

- **`M3.28.3`:** Add `"crates/slipfork-edge/tests/**"` to `paths` (`M3.28.3` requires `critical_edge` assertions and `golden/slipforkd.service` update for `Requires=slipforkd.socket`, `After=slipforkd.socket`, `WantedBy=multi-user.target`, plus `DaemonControl::shutdown`).
- **`M3.28.6`:** Add `crates/slipfork/src/layout.rs`, `crates/slipfork/src/bin/slipforkd.rs`, `crates/slipfork/src/bin/git-remote-slipfork.rs`, `crates/slipfork/src/vaults.rs`, and `crates/slipfork/tests/support/mod.rs` to `paths` so deleting `xdg_runtime_dir` from `Layout::resolve` (`<state_dir>/run`) compiles across all callers.
- **`M3.28.8`:** Drop `[--watch]` and the `watch-flag` acceptance grep (`M3.28.1` locks no watcher, no polling, and no new workspace dependency in `0.1.0`, and `crates/slipfork` is `#![forbid(unsafe_code)]`).
- **`M3.28.9`:** Add `crates/slipfork-ospp/src/pairing.rs`, `crates/slipfork/src/bin/slipforkd.rs`, and `crates/slipfork/tests/support/mod.rs` to `paths` so `BrowserOpener::open_origin(&WorldOrigin)` compiles on `SystemBrowserOpener` and `MarkerBrowserOpener`.
- **`M3.28.11`:** Fix the empty `dag plan` acceptance string from `snow.yml` to `no artifacts or fixtures declared in slipfork.toml` (`[artifacts]`/`[fixtures]` live in `slipfork.toml` per `H-001`, never `snow.yml`), run the `--file` check inside a git checkout, and add `crates/slipfork-engine/src/btrfs_engine.rs` to `paths` if `entry_command` `--wd` is adjusted there rather than `M3.28.4`.
