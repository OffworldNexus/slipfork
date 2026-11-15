# M2.19 docs review — DX lens

- **Node:** `M2.19` (published docs for `0.2.0`)
- **Reviewer:** fresh-context `critic`, lens `dx`
- **Binary:** `target/debug/slipfork` (`--help` for every command; real `doctor`, `snow resolve`, `dag plan`, `up`, `list`, `prune`, `forward`, `daemon unit`, `support`, `git-remote-slipfork`)
- **Files:** `README.md`, `docs/index.md`, `docs/getting-started.md`, `docs/snow-modes.md`, `docs/testing-dag.md`, `docs/agents-and-opencode.md`, `docs/reference/cli.md`, `docs/reference/manifest.md`, `docs/support-matrix.md`, `zensical.toml`
- **Note:** the critic session hit its step cap before writing this file; the doc-writer transcribed its returned findings, re-verified each against the binary/source, and recorded the resolution. Findings marked *unverified* were not applied.

## BLOCKER

### DX-B1 — The published minimal `snow.yml` cannot boot
`getting-started.md` claimed the 4-line `images:` snippet “is the whole requirement”. Verified with the real binary: `slipfork: mode 'local' is not defined in snow.yml (available modes: (none))`, because `slipforkd` hardcodes `WORLD_MODE = "local"`.
**Resolution:** the snippet now includes `modes: { local: {} }` and a named Dockerfile stage comment, the prose names both required parts, and `manifest.md` documents that `up`/`fork` hardcode `local`.

### DX-B2 — The quickstarts are not runnable from a clean repository
`README.md` and `docs/index.md` ran `snow resolve`, `dag plan`, and `up` without creating `snow.yml`/`slipfork.toml`; the repository itself contains neither, so every command failed.
**Resolution:** README now states the prerequisite repository state and links to getting started; index says “In a repository with a `snow.yml` from getting started”. Getting started remains the one page that authors the files.

### DX-B3 — Wrong manifest named for the OpenCode plugin probe
`agents-and-opencode.md` said the plugin probes `<repo>/slipfork.toml`. Source: `coordinator.ts` sets `MANIFEST_FILE = "snow.yml"` and calls `loadSlipforkProjectState` with `snowPath`/`snowFile`.
**Resolution:** corrected to `<repo>/snow.yml`; `slipfork.toml` is described as the DAG/world selector only.

## MAJOR

### DX-M1 — `snow resolve` was described as local
`README.md`, `docs/index.md`, and `cli.md` called `snow resolve` a local diagnostic. `main.rs::resolve_snow` calls `connect_or_spawn`, so it spawns/serves through `slipforkd` and creates runtime state.
**Resolution:** all three pages now say `doctor` and `dag plan` are local; `snow resolve` and every world command go through the daemon.

### DX-M2 — Step 5 examples did not exist in the page’s own manifest
`slipfork redis cache` and `slipfork hook run api:pre-deploy` failed against the §2 manifest, which declared neither `redis_clusters` nor `hooks:`.
**Resolution:** the §2 manifest now declares `redis_clusters: [{ id: cache, version: "8" }]` and an `api` `pre-deploy` hook.

### DX-M3 — `up` was said to start every backing service
The page claimed PostgreSQL, Redis, and SeaweedFS start on first `up`; only declared `resources:` start.
**Resolution:** reworded to “one `crun` OCI container per declared resource”.

### DX-M4 — `share` read as a working feature
The command table said “mint a tunnel lease”; sharing fails closed until the `weaved` registrar lands (`H-006`/M2.22), and the real error’s suggestion to start from the socket unit is only one precondition.
**Resolution:** the table row now says sharing is unavailable until `H-006`/M2.22 and fails closed; README/index lifecycle rows carry the same caveat.

### DX-M5 — `cli.md` promised every flag but omitted real ones
Missing: `snow resolve --file/--mode/--component/--expose-secrets`, `dag plan --file/--repo`, `daemon unit --only/--forward-port`, `forward` positional targets, `secret set --force`, `logs --follow/--lines/--stderr`, `hook run --no-tty`.
**Resolution:** added a command-specific flags table; the command table keeps one verified invocation per command.

### DX-M6 — `--world` was claimed for `fork`
`slipfork fork` takes a positional `<FORK>` and has no `--world`.
**Resolution:** removed `fork` from the `--world` list.

### DX-M7 — Exit-code contract hid child-status propagation
`exec`/`shell`/`db`/`redis`/`hook run` exit with the child status, and their child stdout is not JSON under `--json`.
**Resolution:** documented both, with the `slipfork exec -- false` example.

### DX-M8 — `[world]`/resource/`domains` coverage gaps in the manifest reference
`domains:` was referenced but never declared; `[resources.<kind>.<id>]` showed strategies without the `local`/`test` override shape.
**Resolution:** added the `domains` line and the `[resources.pg_databases.appdb] local = "prefer_remote"` shape.

### DX-M9 — `dag plan` always prints `Rebuild`
Two consecutive runs with the same state dir both printed `Rebuild`, because nothing materializes digests until M3; the page implied `Cached` is user-visible.
**Resolution:** `testing-dag.md` now states every node reports `Rebuild` and `Cached` is only reachable once M3 materializes digests.

### DX-M10 — `worktree.directory` was presented as a knob
The plugin hardcodes `<repo>/.worktrees/<fork>`; the OpenCode directory must match, but changing it does not move Slipfork’s checkout.
**Resolution:** the agents page now says Slipfork always owns `<repo>/.worktrees/<fork>` and the value must be `.worktrees`.

### DX-M11 — “Zero sudo” contradicted the systemd section
The optional unit install uses `sudo` four times and binds port 80.
**Resolution:** taglines now say “zero `sudo` on the default path”; getting started marks systemd as the one optional `sudo` step.

### DX-M12 — Duplicated manifest reference
`snow-modes.md` restated the full generator grammar from `manifest.md`.
**Resolution:** replaced with the user-facing `??`/`!!` contract plus a link; the manifest reference is canonical.

### DX-M13 — Site metadata advertised M3
`zensical.toml` described “parallel snapshot-DAG test orchestration”, which is not shipped.
**Resolution:** replaced with “orthogonal build/fixture planning”.

### DX-M14 — `support-matrix.md` lacks recipe/test pointers
The generated table lists evidence kinds but not the recipe page path or critical-test symbol, so a reader cannot inspect the claim from the page.
**Resolution:** deferred — the renderer lives in `tools/check_support.py`, outside this node’s `paths`. Follow-up: add `recipe_page` link and `critical_test` symbol columns to the generator, then regenerate. The index no longer claims the matrix contains those pointers.

## MINOR (not blocking)

- Version strings say `0.2.0` while `Cargo.toml`/`package.json` remain `0.1.0` until `M2.20`; release sequencing, not a docs edit.
- Internal Rust type/variant names (`SnowError::*`, `ResourceCoordinates::for_world`) remain in user pages for contributor traceability.
- README/index share quickstart and roadmap text; both are short and now caveated.
- `prune --dry-run --yes` previews (dry-run wins); the docs say “preview or reap”, which is accurate for the common call.
- `git-remote-slipfork` first-run error differs from the documented “world not running” output; the documented line is a real run.
- `bun` is not listed as a prerequisite for the plugin test command; the command is in the plugin page only.
