# M2.19 docs review — AI-ergonomics lens

- **Node:** `M2.19` (published docs for `0.2.0`)
- **Reviewer:** fresh-context `critic`, lens `ai-ergonomics`
- **Binary:** `target/debug/slipfork` (all `--help`, `doctor`, `snow resolve`, `dag plan`, `prune` defaults, non-TTY `db`/`shell`/`share`, `daemon unit`, `support`, `git-remote-slipfork`) plus source checks in `main.rs`, `daemon.rs`, `slipfork-dag/src/world.rs`, `slipfork-engine`, and `packages/opencode-slipfork`.
- **Question:** can an AI coding agent act from these pages alone — copy-pasteable invocations, unambiguous world/target selection, explicit failure modes, no unshipped flags or providers?
- **Note:** the critic session hit its step cap before writing this file; the doc-writer transcribed the returned findings, verified them, and recorded the resolution. Deferred items are marked.

## BLOCKER

### AE-B1 — The “whole requirement” manifest cannot resolve
A minimal `images:`-only manifest fails with `mode 'local' is not defined in snow.yml`; an empty `modes: { local: {} }` is mandatory because `slipforkd` hardcodes `WORLD_MODE = "local"`. An agent that copies the snippet cannot reach `snow resolve` or `up`.
**Resolution:** getting started now shows `images:` + `modes.local`, gives both exact failure messages and fixes, and the manifest reference states the hardcoded mode.

### AE-B2 — Quickstarts omit the manifests they need
`README.md`/`docs/index.md` assume `snow.yml` and `slipfork.toml` exist; a clean checkout has neither. `snow resolve`, `dag plan`, and `up` all fail on line 2–4 of the example.
**Resolution:** both pages now state the repository prerequisite and link to getting started, which authors both files.

### AE-B3 — Plugin probe named the wrong file
`agents-and-opencode.md` said the plugin probes `slipfork.toml`; `coordinator.ts` declares `MANIFEST_FILE = "snow.yml"` and stores it as `snowFile`. An agent debugging disabled routing would look at the wrong file.
**Resolution:** corrected; the page also states `slipfork.toml` is the world/DAG selector, not the plugin gate.

## MAJOR

### AE-M1 — Examples that fail against the page’s own manifest
`slipfork redis cache` needs `redis_clusters.cache`, and `slipfork hook run api:pre-deploy` needs an `api` `pre-deploy` hook; the page’s §2 manifest had neither. An agent cannot tell whether the command or its manifest is wrong.
**Resolution:** both are declared in the §2 manifest.

### AE-M2 — Backing-service claim overstated
“Starts PostgreSQL, Redis, and SeaweedFS” reads as unconditional; only declared resources start.
**Resolution:** reworded to one container per declared resource.

### AE-M3 — No agent recipe for DB access
`db`/`redis` require a TTY; the only non-interactive route is `forward` + a host client, which the pages did not connect.
**Resolution:** getting started documents `slipfork forward pg_databases.appdb` followed by host `psql`.

### AE-M4 — Exit codes and JSON ambiguity for world-entry commands
`exec`/`shell`/`db`/`redis`/`hook run` propagate the child status and pass child stdout through; `--json` does not make their output machine-parseable.
**Resolution:** documented explicitly, including `slipfork exec -- false` → exit `1` with no `slipfork:` line.

### AE-M5 — `--world` accepts a bare fork label, `list` prints `<project>:<fork>`
An agent copying `acme:feat-billing` from `list` or an error into `--world` gets `InvalidForkName`.
**Resolution:** the CLI page now states `--world` takes the fork label (`--world feat-billing`) and the `<project>:` prefix is diagnostic; `list` remains the source for worktree/origin.

### AE-M6 — `fork` listed as accepting `--world`
It takes a positional `<FORK>` only.
**Resolution:** removed from the `--world` list.

### AE-M7 — `share` table row implied a working mint
Sharing fails closed until `weaved` lands; the row is what an agent skims first.
**Resolution:** the row now says unavailable until `H-006`/M2.22 and fails closed; README/index carry the caveat.

### AE-M8 — `cli.md` flag coverage incomplete
Real flags for `snow resolve`, `dag plan`, `daemon unit`, `forward`, `logs`, `hook run`, and `secret set --force` were absent.
**Resolution:** added a command-specific flags table; examples stay one per command.

### AE-M9 — `[world]` inference wording was imprecise
“Both keys are inferred when only one choice exists” is wrong for `stage` (a named final stage is inferred even among several).
**Resolution:** the table now states the exact inference and fail-closed conditions; the “optional and inferred when unambiguous” sentence remains.

### AE-M10 — Manifest reference omitted `domains` and the resource-override shape
`domains[].id` was referenced but never declared; `[resources.<kind>.<id>]` omitted its `local`/`test` keys.
**Resolution:** added both, with a `local = "prefer_remote"` example.

### AE-M11 — `dag plan` never shows `Cached` today
The page implied an agent can observe `Cached`/`Rebuild` orthogonality; two consecutive runs both print `Rebuild` until M3 materializes digests.
**Resolution:** page now states this explicitly and points at the critical test for the orthogonality guarantee.

### AE-M12 — `worktree.directory` presented as functional configuration
The plugin hardcodes `<repo>/.worktrees/<fork>`; an agent changing the setting gets a mismatch.
**Resolution:** page now states the CLI owns the path and the setting must be `.worktrees`.

### AE-M13 — “Zero sudo” contradicted the systemd setup
Port-80 unit installation needs `sudo`; the tagline read as an absolute.
**Resolution:** taglines scope it to the default path; the systemd section marks the one optional `sudo` step.

### AE-M14 — Support matrix is not auditable from the page
It lists evidence kinds but not recipe-page paths or critical-test symbols, so an agent cannot verify a tier claim without opening `support/integrations.toml`.
**Resolution:** deferred — the renderer lives in `tools/check_support.py`, outside this node’s `paths`. Follow-up: extend `render_matrix` with `recipe_page` links and `critical_test` symbols, then regenerate. The index no longer claims the page contains those pointers.

## MINOR (not blocking)

- `slipfork --version` reports `0.1.0` until `M2.20` bumps the workspace; pages say `0.2.0` by release intent.
- `site_description` advertised “parallel snapshot-DAG test orchestration” (M3) — fixed to “orthogonal build/fixture planning”.
- `slipfork share`’s real error suggests starting the socket unit, but sharing still needs `weaved`; the page now explains the second precondition.
- `git-remote-slipfork` first-run output differs from the documented “world not running” line; both are real states.
- `forward` blocks until Ctrl-C; pages say so, but an agent calling it synchronously will hang — prefer `exec` for command execution.
- `logs --json` emits streamed chunks; the page does not claim otherwise.
- Internal Rust identifiers remain in the two reference pages for traceability.
