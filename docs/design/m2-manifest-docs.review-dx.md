# Review record: M2.23 manifest/mode docs purge (dx lens)

- **Node:** `M2.23` — purge stale M1 manifest/snow-mode docs after `M2.4`/`M2.5`.
- **Scope reviewed:** `README.md`, `docs/index.md`, `docs/getting-started.md`, `docs/snow-modes.md`, `docs/reference/manifest.md`, `docs/reference/cli.md`, `docs/agents-and-opencode.md`.
- **Lenses applied:** `dx` (author self-review against `dx-first`, since the standalone dx critic had not returned a final report at close-out) and `ai-ergonomics` (fresh-context critic, findings applied below). A `cruft` critic also reviewed the seven pages; its findings are folded in.

## Findings applied

| Severity | Finding | Resolution |
|---|---|---|
| BLOCKER | `snow-modes.md` example declared component `mailpit` with no `images[]` entry → `unknown image 'mailpit'` | added a prebuilt image (`registry`/`repository`/`tag`) and documented both `images[]` variants in the manifest reference |
| BLOCKER | `run_command` override described as "runs", but component supervision is daemon-owned | now says "resolves `<cmd>`; supervision lands with `slipforkd` in `0.2.0`" |
| BLOCKER | `Rust 1.88+` contradicted `rust-version = "1.94"` | corrected to `1.94+` |
| BLOCKER | bare repositories claimed to be fixable via `project = "<slug>"` | split: bare repos must be cloned; submodules/`--separate-git-dir` take the override |
| BLOCKER | OpenCode plugin configured as a single `.ts` file (V2 skips file paths) and relative to the wrong directory | `{ "package": "../packages/opencode-slipfork", "options": {...} }`; paths resolve from the config file |
| BLOCKER | quickstart invited copy-paste that fails on a fresh checkout | each quickstart states it runs inside your project after the tutorial creates the manifests; only `doctor` works in an empty checkout |
| MAJOR | `--json` shapes, exit codes, `--file` resolution, and `--component` errors undocumented | added one JSON-shape table, one exit-code table, and explicit defaults |
| MAJOR | `dag plan` implied `Cached` was reachable in `0.1.0` | states planning only reads the cache; every node prints `Rebuild` until M3 |
| MAJOR | `snap` failure modes had no errno/text guidance | documented exit `1`, `errno 95` (destination not CoW) vs `errno 18` (cross-device) |
| MAJOR | `domains::<ID>` described as never local, contradicting `ADR-0005` §1 | scoped to `0.1.0`; notes the edge will derive local `domains::<id>` in `0.2.0` |
| MAJOR | `resources::app.url` port was ambiguous vs `ingresses[].target.port` | clarified: origin port (`80` portless), container ports never appear |
| MAJOR | benchmark comment "under a millisecond" contradicted the 44.4 ms PostgreSQL row | "in milliseconds" |
| MAJOR | remote `OSPP v1` provider described as wired | states the local `0600` vault is the only provider `snow resolve` reads today |
| MINOR | `??` literal must be JSON string; generator encodings; `--from`/`--workspace` defaults; `[resources]` inert-but-parsed | all stated explicitly |

## Deliberate rejections

- **Re-adding a `make bench` reproduce pointer:** `poc/run_benchmark.py` still drives the M1 `fork`/`exec`/`drop` CLI, which refuses to run in this tree; `M2.15` aligns the harness with the `0.2.0` CLI. The committed report is cited by path instead.
- **Documenting `components[].size_hint`:** parsed but unread anywhere in the workspace; documenting it would advertise a no-op field.
