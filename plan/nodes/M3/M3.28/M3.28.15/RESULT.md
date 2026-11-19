# RESULT — M3.28.15: delete the unimplemented tunnel-sharing surface

Base: `d24a6c5` (`release/0.1.0`) · Node: `plan/nodes/M3/M3.28/M3.28.15/NODE.md` · Not committed.

## Outcome

The whole tunnel-sharing surface is gone: `slipfork share` no longer parses (`exit=2`), the
`DaemonControl` seam, the fail-closed registrar, the world capability traits, the `Shared` origin
kind, the support-registry tier, and every published claim were deleted in one sweep. No rename, no
alias, no `#[deprecated]`, no "coming soon" teaser anywhere published.

## Deletion size

`git diff --numstat d24a6c5 --` over the files this node owns outright
(`crates/slipfork-world/**`, `crates/slipfork-edge/**`, `crates/slipfork-dag/src/support.rs`,
`crates/slipfork-snow/tests/critical_snow.rs`, `crates/slipfork/tests/critical_cli.rs`):

```console
$ git diff --shortstat d24a6c5 -- crates/slipfork-world crates/slipfork-edge \
    crates/slipfork-dag/src/support.rs crates/slipfork-snow/tests/critical_snow.rs \
    crates/slipfork/tests/critical_cli.rs
 15 files changed, 120 insertions(+), 1480 deletions(-)
$ git diff --numstat d24a6c5 -- <same set> | awk '{a+=$1; d+=$2} END {print "added="a" deleted="d}'
added=120 deleted=1480
```

That is **1,480 deleted lines** (net −1,360) for the owned surface, including the deleted
`crates/slipfork-edge/src/tunnel.rs` (32 lines). The node additionally edits five shared files
whose diffs also carry earlier nodes' work: `crates/slipfork/src/{cli,main,daemon}.rs`,
`crates/slipfork/src/bin/slipforkd.rs`, `README.md`, plus the four docs files and two ADRs below
(~−55 lines attributable here). Whole-worktree shortstat at this base is
`37 files changed, 440 insertions(+), 1680 deletions(-)` and includes other nodes' edits.

## Removed symbols

| Crate / file | Deleted |
|---|---|
| `crates/slipfork/src/cli.rs` | `Share { stop, world }` clap variant (and `--stop`/`--world` args) |
| `crates/slipfork/src/main.rs` | `Commands::Share` arm, `fn share`, "tunnel lease" module-doc phrase |
| `crates/slipfork/src/daemon.rs` | `DaemonService::{share, unshare}`, `UnavailableTunnelRegistrar` wiring, `SharedWorldInfo`/`UnavailableTunnelRegistrar` imports, 4-arg `WorldRegistry::new` |
| `crates/slipfork/src/bin/slipforkd.rs` | `UnavailableTunnelRegistrar` composition-root doc line |
| `crates/slipfork-edge/src/tunnel.rs` | **module deleted** (`UnavailableTunnelRegistrar`) |
| `crates/slipfork-edge/src/lib.rs` | `pub mod tunnel`, `pub use tunnel::UnavailableTunnelRegistrar`, `RouteDecision` re-export, `SharedWorldInfo` re-export, `EdgeError::{NoForwardListener, TunnelUnavailable, SharedHostConflict}` |
| `crates/slipfork-edge/src/registry.rs` | `WorldRegistry::{share_world, unshare_world, planned_forward_port, local_origin}`, `registrar`/`planned_forward_port` fields, `RouteDecision`, `SharedWorldInfo`, `ActiveShare`, `RegistryState::{share, take_share, insert_share}`, `shared_hosts`/`shares` maps, `is_localhost_host`, `normalize_shared_host`, the `308`/`421` branch in `resolve`, `upgrade` parameter |
| `crates/slipfork-edge/src/ingress.rs` | `Redirect`/`Misdirected` dispatch, `write_redirect`, `write_misdirected`, `write_status` |
| `crates/slipfork-edge/src/daemon.rs` | `DaemonRequest::{Share, Unshare}`, `DaemonReply::{Shared, Origin}`, `DaemonControl::{share, unshare}`, `DaemonErrorKind::{NoForwardListener, TunnelUnavailable}` + their `From<&EdgeError>` arms |
| `crates/slipfork-edge/src/daemon/{client,server}.rs` | the matching client calls, server dispatch arms, and imports |
| `crates/slipfork-world/src/lib.rs` | `TunnelRegistrar`, `TunnelLease`, `TunnelError`, `TunnelRequest`, `LocalOrigin`, `SharedOrigin`, `WorldOrigin::Shared`, the `kind`-tagged `Wire` enum, the IPv6 `host()` branch; `WorldOrigin` is now `struct WorldOrigin { url }` with one constructor and `{"url": …}` wire shape |
| `crates/slipfork-world/tests/support/mod.rs` | `InMemoryTunnelRegistrar`, `InMemoryLease`, `assert_tunnel_registrar_conformance` |
| `crates/slipfork-world/tests/critical_world.rs` | registrar-conformance test, shared-origin host/serde cases, `WorldOrigin::Local(_)` match |
| `crates/slipfork-dag/src/support.rs` | `IntegrationCategory::Tunnel` + its doc sentence |
| `crates/slipfork/tests/critical_cli.rs` | `share_blocked_while_unshare_is_an_idempotent_no_op`, `"share"` in the read-verb matrix, stale header phrase |
| `crates/slipfork-edge/tests/critical_edge.rs` | `InMemoryTunnelRegistrar`, `InMemoryLease`, `forward_decision`, the `308`/`421` tests, the share/unshare/release tests, the share case of `daemon_control_preserves_typed_errors`, `TestControl::{share, unshare}`, `registry_over`; the real proxy/stream/WebSocket tests were **retargeted** to `acme.localhost` (renamed `proxy_*`/`websocket_*`) rather than dropped |

Docs: `docs/reference/cli.md` (command row, `--world` lists, failure-mode bullet),
`README.md` (lifecycle bullet, planned bullet), `docs/index.md` (lifecycle row, planned bullet),
`docs/design/target-dx.md` (`struct WorldOrigin { url }` tombstone wording), and `ADR-0005`
annotated **superseded for `0.1.0` by `ADR-0011`** with every `WorldOrigin::{Local,Shared}`,
`share`, `TunnelRegistrar`, `shared_hosts`, `308`/`421` sentence rewritten in the future tense.
`docs/VISION.md` was **not** edited (already future-framed) and is the only remaining "tunnel
sharing" mention.

## Support registry

`support/integrations.toml` carried **no** tunnel entry (verified before deleting the tier:
`grep -n 'category' support/integrations.toml` lists only `world-engine`, `secrets`, `cli`, `vcs`).
Only the orphan `IntegrationCategory::Tunnel` enum variant was removed; no registry entry was
invented to delete.

## Edits outside the node's globs (acceptance-forced, minimal)

1. `crates/slipfork-world/Cargo.toml` — one-line `description` no longer advertises `TunnelRegistrar`.
2. `crates/slipfork-snow/tests/critical_snow.rs` — **7 deleted lines**: the block asserting
   `WorldOrigin::shared(...)` coordinates. This is a protected critical test and the only way to
   keep the workspace compiling once `WorldOrigin::shared` is deleted (invariant 8: delete the test
   of a deleted behaviour in the same commit).
3. `docs/knowledge/LESSONS.md` + `docs/decisions/ADR-0011-*.md` — one sentence each reworded to keep
   the accurate "removed in `0.1.0`" record while dropping the literal `slipfork share` /
   `tunnel-sharing` strings that the inverted acceptance grep forbids.

**`critical_guard.py` reports #2 as `VIOLATION: not covered by any node path`** — the plan needs
`crates/slipfork-snow/tests/**` added to `M3.28.15.paths` (or the conductor's `--report` ack). The
change is 7 deletions of a deleted API's assertions; nothing unrelated was touched.

## Verified

```console
$ cargo run -q -p slipfork -- share; echo "exit=$?"
error: unrecognized subcommand 'share'

  tip: a similar subcommand exists: 'secret'

Usage: slipfork [OPTIONS] [COMMAND]

For more information, try '--help'.
exit=2
$ cargo run -q -p slipfork -- --help | grep -q '^  share' && echo STILL-SHARE || echo share-gone
share-gone
$ ! grep -rn 'TunnelRegistrar\|UnavailableTunnelRegistrar\|WorldOrigin::Shared\|slipfork share\|shared_hosts' \
    crates/ docs/reference/ README.md support/ && echo no-tunnel-residue
no-tunnel-residue          # grep exit=1 (zero matches)
$ ! grep -rn 'slipfork share\|tunnel shar' docs/ README.md && echo no-share-docs
# grep exit=0 with exactly one survivor: docs/VISION.md:35, the sanctioned future-intent line.
# `grep ... --exclude=VISION.md` exits 1 (zero matches).
$ cargo test -q -p slipfork-world --test critical_world
running 14 tests
test result: ok. 14 passed; 0 failed
$ cargo test -q -p slipfork-edge --all-targets
running 4 tests ... test result: ok. 4 passed; 0 failed
running 35 tests ... test result: ok. 35 passed; 0 failed
$ cargo test -q -p slipfork-dag --test critical_support
running 4 tests
test result: ok. 4 passed; 0 failed
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
test result: ok. 23 passed; 0 failed; 0 ignored
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (11 doc(s) match real CLI subcommands: published pages plus support/demo/README.md)
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)
$ cargo fmt --all -- --check
(clean)
$ make docs-check
No issues found / check_docs_honesty: OK
$ uv run --no-project tools/check_stub_adapters.py --check all
check_stub_adapters: OK (91 production file(s), 6 composition root(s))
$ uv run --no-project tools/guard.py
guard: OK (598 file(s) checked)
$ uv run --no-project tools/plan.py validate
plan OK
$ uv run --no-project tools/critical_guard.py --base d24a6c5 --node M3.28.15
M  crates/slipfork-edge/tests/critical_edge.rs   [authorized by crates/slipfork-edge/tests/**]
M  crates/slipfork-snow/tests/critical_snow.rs   [VIOLATION: not covered by any node path]
M  crates/slipfork-world/tests/critical_world.rs [authorized by crates/slipfork-world/tests/**]
M  crates/slipfork/tests/critical_cli.rs         [authorized by crates/slipfork/tests/**]
critical-guard: FAILED: 1 unauthorized protected test change(s) in M3.28.15.
```

The single critical-guard finding is the acceptance-forced 7-line deletion in
`crates/slipfork-snow/tests/critical_snow.rs` described above; it needs the node's `paths`
widened (plan edit by the conductor) or an explicit `--report` acknowledgement.

`make check`: **fails at `rs-lint`'s rustdoc step on a pre-existing, out-of-scope broken
intra-doc link**, not on this node's surface:

```console
$ make check
plan OK
guard: OK (599 file(s) checked)
ruff check/format: All checks passed! / 15 files already formatted
pytest -q tools/tests: 150 passed
check_support: OK (10 integration(s); …)
bun test packages/opencode-slipfork/tests: 16 pass, 0 fail
cargo fmt --all -- --check                      # clean
cargo clippy --workspace --all-targets -- -D warnings   # Finished
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
error: unresolved link to `tests::the_remote_recipe_host_header_round_trips`
  --> crates/slipfork/src/open.rs:15:43
make: *** [Makefile:59: rs-lint] Error 101
```

`crates/slipfork/src/open.rs` is **untracked and not this node's file** (`git status` shows
`?? crates/slipfork/src/open.rs`; it is absent from `git diff d24a6c5`, and it was already present
before this node started). The link targets a `#[cfg(test)]` item, which `cargo doc` does not build.
Fixing it belongs to the node that owns `open.rs` (a one-line doc-link edit) — I did not touch it.
To prove the rest of `rs-test` is unaffected I ran, green:

```console
$ cargo test -q --workspace --lib --bins     # all targets ok
$ cargo test -q --workspace --doc            # all targets ok
$ cargo test -q -p slipfork-snow --test critical_snow   # 12 passed
```

Also note `cargo fmt --all` had to be re-run after an import edit; the final tree is fmt-clean and
clippy-clean.

`make check` does **not** yet invoke `tools/check_stub_adapters.py` (`stub-check` exists in the
Makefile but is not in the `check:` dependency list). Run explicitly as the bonus it is green:
`check_stub_adapters: OK (91 production file(s), 6 composition root(s))`, exit 0. Wiring
`stub-check` into `check:` is a separate one-line Makefile change and was **not** made here.

## Handoff

- **`make check` is red only on the pre-existing `crates/slipfork/src/open.rs:15` rustdoc link**
  (another node's untracked file); every earlier `make check` stage is green, and
  `cargo test --workspace --lib --bins`, `cargo test --workspace --doc`, and `critical_snow` pass.
- **`critical_guard` needs `crates/slipfork-snow/tests/**` in `M3.28.15.paths`** to authorize the
  7-line deletion of `WorldOrigin::shared` assertions (compilation forces it).
- Nothing committed; `slipfork-world/Cargo.toml` and `critical_snow.rs` are the only out-of-glob
  code edits and both are minimal.

One earlier `critical_cli` run failed
`bare_slipfork_and_status_render_one_dashboard_and_json_stays_pure` with a host-state flake
(`world init pid … survived SIGTERM and SIGKILL for 15.0s`); that test passed in isolation and the
full suite then passed 23/23 with no code change — no assertion was weakened.