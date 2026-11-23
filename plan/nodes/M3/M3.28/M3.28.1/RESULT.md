# M3.28.1 RESULT — `0.1.0` UX DX Blueprint, `ARCHITECTURE.md` Seams, and `ADR-0011` (Round 2 Converged)

- **Status:** `done`
- **Node:** `M3.28.1` (`release/0.1.0`, base `1d4c08e`, Round 2/2)
- **Deliverables:**
  - `docs/design/dx-0.1.0-ux.md`: single normative home for the `0.1.0` CLI command table (§10), CLI `--json` schemas (§3 & §10), interactive transcripts, fail-closed remedies, and all 9 locked decisions.
  - `docs/decisions/ADR-0011-port-80-daemon-install-and-runtime-dir.md`: records `slipfork daemon install`, `:80` system socket activation (`M3.28.6` adding `Requires=slipforkd.socket`, `After=slipforkd.socket`, `WantedBy=multi-user.target` to `render_service_unit` + `golden/slipforkd.service`), `:0 -> :80` `DaemonControl::shutdown` handover, `<state_dir>/run` via `Layout::for_account`, and `doctor` `IngressProbe`.
  - `ARCHITECTURE.md`: unified Composition Root, Mermaid DI diagram, and §3 wire/trait contracts (`IngressInfo`, `DaemonStatus`, `WorldSummary`, `WorldProcessReport`, `WorldServiceSummary`, `struct WorldOrigin(LocalOrigin)`, `WorldRootfs`/`OciRootfs` cache + millis, `WorldEngine::{world_processes, world_lineage}`, narrow `UnitInstaller`, `UserIdentity::invoking_account`, `Layout::for_account`, `crates/slipfork/src/browser.rs`).
  - `docs/design/target-dx.md`: duplicate command table and `--json` schema block deleted in favor of links to `docs/design/dx-0.1.0-ux.md` §10.

## Round-2 Quad Review Findings Closed (1–14)

1. **Single table & `--json` schema home:** `docs/design/dx-0.1.0-ux.md` §10 is sole authority (`grep -c '| \`slipfork' docs/design/target-dx.md` = `0`).
2. **Unified wire model vs CLI projection:** `DaemonStatus { pid, ingress: IngressInfo { addrs, port }, forward_port, worlds }` (`port` validated in `addrs`), `WorldSummary { world: WorldId, parent: Option<WorldId>, state, origin, worktree, lineage }`, `WorldProcessReport { lineage, services }`, `WorldServiceSummary { world, id, kind, state: ServiceState, pid, port, uptime_secs }`. Deleted daemon-wide lineage cell; `WorldRootfs`/`OciRootfs` carry `cache` + `millis`; `port` + `services` + `ServiceState` enum everywhere; `uptime_secs` stamped by engine; CLI `--json` `{"ok": true, ...}` + `"origin": origin.url()` projection documented once.
3. **Collapsed `WorldOrigin`:** Untagged `struct WorldOrigin(LocalOrigin)` (no one-variant tagged enum).
4. **Identity:** Added `UserIdentity::invoking_account(&self) -> Result<Option<String>, String>` in `crates/slipfork/src/user.rs` (sole reader of `SUDO_USER` under fixed `PATH`, validated via `lookup_account`).
5. **`Layout`:** Added pure `Layout::for_account(&ResolvedAccount, ...)`; `Layout::resolve` = resolve invoking account + `for_account`.
6. **Narrow `UnitInstaller`:** Replaced `is_root()` + stringly `run(program, args)` with `read_file`, `write_file_atomic`, `daemon_reload`, `apparmor_replace`, `enable_now(&[ManagedUnit])`, `restart_service`; pure install orchestration takes `is_root: bool` and `Option<&str>` (no `--force`).
7. **`BrowserOpener`:** Env-free port in `crates/slipfork-ospp` (preserving zeroizing `LivePairingUrl`), single `SystemBrowserOpener` in `crates/slipfork/src/browser.rs` wired by both binaries; headless fails closed naming `slipfork open --remote`.
8. **`open --remote` & single non-`:80` notice owner:** Remote endpoint uses `127.0.0.1:<ingress.port>` (local constant `8080`); `dx-0.1.0-ux.md` §2 rule 4 is the single normative owner naming `status`, `up`, `fork`, `restart`, `list`, and `open` (`target-dx.md` and `ADR-0011` link to it).
9. **Flat AppArmor alternation:** One `<rel_dir>/slipfork{,d,-world-init}` alternative per `--bin-dir` (no nested `{a/{b,c}}`).
10. **Fork naming:** `validate_fork_name(&sanitised, &snow_domains)` receives the checkout's real `snow.yml` `domains[].id` slice (never `&[]`), with project-name equality checked separately.
11. **`ADR-0011` tense & `M3.28.6` scope:** `render_service_unit` + `tests/golden/slipforkd.service` changes (`Requires=`, `After=`, `WantedBy=multi-user.target`) written as `M3.28.6` future work.
12. **One boot/restart pipeline:** `up` runs the boot pipeline when unbooted/`stopped`/`failed` (`changed: false` when `running`); `restart` stops components + `crun` services in place, then runs that exact same boot pipeline (`changed: true`).
13. **`slipfork-dag` in `ARCHITECTURE.md`:** Removed `DagPlanner`, `InputHasher`, `DigestCache`, `SupportRegistry`, and `"parked for Phase 2 M3.2"` from the `0.1.0` composition-root diagram/prose.
14. **MINORs:** Documented trailing `-` on warm `base <key12> -`, locked `miss-offline` token for `ServiceCache::MissOffline`, gave `slipfork prune` its own table row, wrote full 64-hex digests in `--json`, and removed `M4.1`/`H-006` teasers from `dx-0.1.0-ux.md` and `target-dx.md`.

## Verified `# Acceptance` & Round-2 Output

```console
$ test -s docs/design/dx-0.1.0-ux.md && echo present
present
$ for t in 'slipfork status' 'slipfork restart' 'slipfork ps' 'slipfork open --remote' 'user.name=slipfork' 'slipfork daemon install' '<state_dir>/run' '127.0.0.1:80' 'lineage'; do grep -qF "$t" docs/design/dx-0.1.0-ux.md || { echo "missing: $t"; exit 1; }; done; echo tokens-ok
tokens-ok
$ test -s docs/decisions/ADR-0011-*.md && grep -qi 'composition root' ARCHITECTURE.md && echo arch-ok
arch-ok
$ ! grep -qE '\[(derived|guessed)\]|TODO|TBD' docs/design/dx-0.1.0-ux.md ARCHITECTURE.md docs/decisions/ADR-0011-*.md && echo no-placeholders
no-placeholders
$ ! grep -rn 'slipfork share\|shared_hosts' docs/design/dx-0.1.0-ux.md docs/design/target-dx.md && echo no-share
no-share
$ grep -c '| `slipfork' docs/design/dx-0.1.0-ux.md docs/design/target-dx.md
docs/design/dx-0.1.0-ux.md:25
docs/design/target-dx.md:0
$ ! grep -rn 'readiness_port\|DaemonStatus.lineage\|WorldOrigin::Shared\|slipfork share' docs/design/*.md ARCHITECTURE.md docs/decisions/ADR-0011*.md && echo one-model
one-model
$ uv run --no-project tools/guard.py
guard: OK (542 file(s) checked)
$ uv run --no-project tools/plan.py validate
plan OK
$ uv run --no-project tools/critical_guard.py --base 1d4c08e --node M3.28.1
critical-guard: 1d4c08e..working tree, 41 changed file(s)
  no change to a protected critical test
$ make docs-check
uvx --offline zensical build --strict
Build started
No issues found
Build finished in 0.21s
uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)
```

## Downstream Node Alignment Notes for `lead` (`plan.py edit`)

- **`M3.28.3`:** Add `"crates/slipfork-edge/tests/**"` to `paths`; wire `IngressInfo`, `WorldSummary` (`parent: Option<WorldId>`, `lineage`, no `processes`), `WorldProcessReport`, `WorldServiceSummary` (`port`, `ServiceState`), and `DaemonControl::shutdown` (no `DaemonStatus.lineage`).
- **`M3.28.6`:** Add `crates/slipfork-edge/src/unit.rs`, `crates/slipfork-edge/tests/golden/slipforkd.service`, `crates/slipfork/src/{layout.rs,vaults.rs}`, `crates/slipfork/src/bin/{slipforkd.rs,git-remote-slipfork.rs}`, and `crates/slipfork/tests/support/mod.rs` to `paths`.
- **`M3.28.8`:** Drop `[--watch]` and `watch-flag` grep (`0.1.0` has no watcher/polling/new deps).
- **`M3.28.9`:** Add `crates/slipfork/src/browser.rs`, `crates/slipfork-ospp/src/pairing.rs`, `crates/slipfork/src/bin/slipforkd.rs`, and `crates/slipfork/tests/support/mod.rs` to `paths`.
- **`M3.28.11`:** Add `crates/slipfork-dag/Cargo.toml` to `paths` (`fs` feature removal), and scope `planner-swept` grep (`critical_dag.rs` is protected and calls `DagPlanner::plan` in-memory).
- **Round-2 confirmation fix:** Replaced stale `WorldOrigin` enum comment on `ARCHITECTURE.md:560` with `// Some when the world's single local origin needs the in-netns :80 bridge` (`no-variant-residue` verified).
