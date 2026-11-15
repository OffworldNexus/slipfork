# Result M2.18

**Status:** done — `@slipfork/opencode` now takes explicit `SlipforkProjectState`, routes through the real daemon-served CLI, and is covered end to end against the compiled `slipfork` binary.

## What changed (`packages/opencode-slipfork/**` only)

- **`src/cli.ts` (new):** one declaration of the CLI surface the plugin drives (`fork <FORK> [--base]`, `drop <FORK>`, `exec --world --cwd -- <argv>`, `--json list`), plus fail-closed JSON decoders (`parseForkedWorld`, `parseWorldList`) and the host-shell command renderer. `--snow-file` is gone; the plugin never resolves or injects `snow.yml` envs.
- **`src/coordinator.ts`:** typed `SlipforkProjectState { repoRoot, snowFile, configError? }` built once by `loadSlipforkProjectState` through an injected `ManifestProbe`; `existsSync`/filesystem probes are gone from the gateway. Worktree `create` calls `slipfork fork` (the CLI atomically owns `.worktrees/<fork>`, its branch, and the CoW world) and registers `{ forkId, cwd, url }` from the fork's `--json` origin. Shell routing only rewrites checkouts the plugin owns (plus CLI-resolved `.worktrees/<fork>` checkouts) via `slipfork exec --world <fork> --cwd <cwd> -- …`; the 2-line `<context key="slipfork-world">` note names the fork and its single origin. Missing manifest → structured `no-manifest`; unreadable manifest → structured `config-error`; both surface a `<context key="slipfork-project">` remediation note instead of a silent skip. Teardown (`releaseAllWorlds`) is idempotent, never throws, and returns warnings.
- **`src/index.ts`:** composition root wires `StatManifestProbe` + `CliSlipforkGateway` (no ambient `cwd` probing: every spawn runs in the project root), registers the three hooks, and releases worlds from the plugin cleanup callback (OpenCode V2 has no session teardown hook; the plugin cleanup is the teardown boundary).
- **`tests/coordinator.critical.test.ts`:** rewritten around the injected gateway/state: create → route → idempotent drop, fork failure leaves nothing bound, `no-manifest`/`config-error` results, teardown warnings, list filtering, command-line shape (no `snow.yml`, only `SLIPFORK_ACTIVE_WORLD`).
- **`tests/cli-contract.critical.test.ts` + `tests/fixtures/cli-contract.json` (new):** fixture recorded from the real `slipfork` `--help`/`--json` output; the suite asserts the plugin constants, the fixture, and live binary help agree, so a CLI rename fails the suite.
- **`tests/e2e.critical.test.ts` (new):** drives `default.setup` against compiled `slipfork` (+ auto-spawned `slipforkd`, `slipfork-world-init`, pinned `crun`) in a disk-backed scratch repo under `~/.cache` (short path for AF_UNIX): fork `feat-live`, assert `http://feat-live.acme-live.localhost:<port>`, run a shell command in `.worktrees/feat-live/api` through `slipfork exec` (asserts `$PWD` is the worktree `cwd` and `$BASE_URL` equals the world origin), fetch the same origin from the host, then dispose (drop) twice (second is a no-op) and assert the checkout is gone.

## Acceptance (exact commands, final tree)

```text
$ cargo build -p slipfork
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.09s

$ make ts-test
bun test packages/opencode-slipfork/tests
bun test v1.4.4 (38afee6c8)
 20 pass
 0 fail
 110 expect() calls
Ran 20 tests across 3 files. [4.74s]

$ bun test packages/opencode-slipfork/tests/e2e.critical.test.ts
bun test v1.4.4 (38afee6c8)
 5 pass
 0 fail
 23 expect() calls
Ran 5 tests across 1 file. [5.47s]

$ ! rg -q "existsSync" packages/opencode-slipfork/src; echo $?
0
$ rg -q "SlipforkProjectState" packages/opencode-slipfork/src; echo $?
0

$ make check
...
uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)
MAKE_CHECK_EXIT=0

$ uv run --no-project tools/guard.py
guard: OK (361 file(s) checked)

$ uv run --no-project tools/critical_guard.py --base e5ac58f --node M2.18
critical-guard: e5ac58f..working tree, 62 changed file(s)
  M  packages/opencode-slipfork/tests/coordinator.critical.test.ts  [authorized by packages/opencode-slipfork/**]
  A  packages/opencode-slipfork/tests/cli-contract.critical.test.ts  [added (always allowed)]
  A  packages/opencode-slipfork/tests/e2e.critical.test.ts  [added (always allowed)]
critical-guard: OK: authorized by M2.18
```

Note: one earlier `make check` run failed a Rust engine test (`slipfork-engine --test critical_oci_runtime`,
`psql create table` exiting 2 under memory pressure from leftover test daemons). That test passes in
isolation and the final full `make check` exited 0; it is unrelated to this node's TypeScript-only
changes.

## Notes / observations (outside this node's paths)

- `package.json` declares no scripts and no dependencies, so there is nothing for `bun install` to resolve; `bun test` runs the TS sources directly.
- `docs/agents-and-opencode.md` still says the gateway is "updated in M2.18" and documents `wt-<slug>` fork names; the shipped plugin derives the fork name from the OpenCode-suggested worktree directory (`.worktrees/<fork>`). Docs are outside `packages/opencode-slipfork/**`, so they were not edited here.
- This host's zero-setup auto-spawn (no explicit `--state-dir`/`--runtime-dir`, with `$XDG_RUNTIME_DIR` set) reproducibly fails world init with an overlayfs `mount` `EINVAL`; the same daemon started directly, and the CLI with explicit roots, boot worlds fine. The e2e therefore uses the documented default roots with the runtime on disk (`<state_dir>/run`, `$XDG_RUNTIME_DIR` unset), which is the same zero-setup path the repo's Rust critical test exercises. Worth a separate CLI/engine look.
