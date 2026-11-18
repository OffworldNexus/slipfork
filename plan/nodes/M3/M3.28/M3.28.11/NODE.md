---
id: M3.28.11
title: "Delete the dag CLI, sweep unreachable dag types, quiet world entry, per-component log prefixes, [resources] behind -v"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.28.10]
paths:
  - crates/slipfork/src/cli.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/tests/critical_cli.rs
  - crates/slipfork/tests/critical_slug.rs
  - crates/slipfork/Cargo.toml
  - "crates/slipfork-dag/src/support.rs"
  - docs/reference/cli.md
  - docs/reference/manifest.md
  - "docs/testing-dag.md"
  - docs/index.md
  - "docs/getting-started.md"
  - README.md
  - AGENTS.md
  - zensical.toml
  - support/integrations.toml
  - ARCHITECTURE.md
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-17T16:33"
updated: "2026-11-18T04:34"
---

# Goal

Owner ruling: `dag` is gone from `0.1.0` — delete the subcommand and its unreachable machinery in the same sweep, delete every published mention of it in the same commit (`make check` runs `tools/check_docs_honesty.py`, which fails if published docs name a subcommand `--help` does not list), and kill the three remaining small lies the owner hit: `getcwd() failed` noise on world entry, unattributed log lines, and internal `[resources]` jargon on the happy path.

# Context

- Fact 6: `slipfork dag plan` printed **nothing** for a project with no artifacts — and the owner's ruling is that the command has almost no `0.1.0` value at all. The lineage facts it would have shown now live on `slipfork up` / `slipfork ps` / `slipfork status` / `up -v` (`M3.28.8`, `M3.28.9`, `M3.28.7`); the plan view is parked to `0.2.0` (`slipfork test --dry-run`, recorded on `M3.2`). Locked decision 8 of `docs/design/dx-0.1.0-ux.md` is the authority.
- Fact 7: `slipfork shell` printed `sh: 0: getcwd() failed: No such file or directory` before the prompt; the entry wrapper starts on a host path that only exists inside the world's mount namespace.
- `slipfork logs` interleaves sources with no attribution.
- Fact 8: `[resources] demo: prefer_remote -> local (not_offered)` is a developer diagnostic on a healthy boot.

# Design / Invariants

1. **`dag` is deleted, not renamed and not deprecated:** remove the `Commands::Dag` variant and `DagAction` from `crates/slipfork/src/cli.rs`, the `run_dag_*`/plan renderer and the `DagPlanner`/`FsDigestCache`/`FsMerkleHasher`/`SupportRegistry`/`Evidence` imports from `crates/slipfork/src/main.rs`, and every `dag` case from `crates/slipfork/tests/critical_cli.rs`. `slipfork dag` must exit `2` as an unknown subcommand. No `slipfork plan`, no alias, no `#[deprecated]`, no hidden variant.
2. **Sweep only what is genuinely unreachable:** `ResourcePlan`/`ResourceStrategy`/`RemoteResourceOffer`, `SlipforkConfig`, `TestSpec`, the fixture/artifact manifest model **and the whole planner/support machinery stay** — `DagPlanner`, `FsDigestCache`, `FsMerkleHasher`, `SupportRegistry`, `Evidence`/`EvidenceField` are exercised by the protected critical tests `crates/slipfork-dag/tests/critical_dag.rs` and `critical_support.rs`, so they are *not* unreachable and must not be deleted, weakened, or `#[ignore]`d. They are the `0.2.0` `slipfork test` engine, retained unshipped. Delete only what dies with the CLI: the plan renderer and its formatting helpers in `crates/slipfork/src/main.rs`, and the now-unused `features = ["fs"]` on the `slipfork-dag` dependency in `crates/slipfork/Cargo.toml` (verify `project.rs`/`daemon.rs` still compile without it). Record the exact kept/deleted list in `RESULT.md`.
3. **Docs truth in the same commit:** delete the `dag plan` rows and the `doctor` + `dag plan` half-sentence in `docs/reference/cli.md`; delete `docs/testing-dag.md` **and** its `zensical.toml` nav entry (`"Orthogonal DAG planner"`); remove the `dag plan` sentences/links from `docs/index.md`, `docs/getting-started.md`, `README.md`; fix `AGENTS.md` line 3's claim that `0.1.0` ships "the `dag plan` planner". `M3.28.14` publishes the replacement surface — here you only tell the truth by deletion.
4. World entry is quiet: the wrapper `chdir`s to a path valid inside the world before `exec`, so `getcwd() failed` never appears in `shell`, `exec`, `db`, or `hook run`. Interactive `shell` prints one honest line: `world rootfs is the app stage (no host tools; use slipfork ps / logs / open)`.
5. `logs` prefixes every line with `<component|hook>:` by default when more than one source is selected; a single named target stays unprefixed; `--json` unchanged.
6. `[resources] …` lines print only with `-v/--verbose` (already a global flag from `M3.28.8`) or via `--json`. A healthy boot prints the actionable block plus the single lineage line only.
7. Delete, never deprecate: no env switch, no second spelling, no dead rendering branch left behind.

# Acceptance

```console
$ cargo run -q -p slipfork -- dag plan; echo "exit=$?"
error: unrecognized subcommand 'dag'
exit=2
$ cargo run -q -p slipfork -- --help | grep -q dag && echo STILL-DAG || echo dag-gone
dag-gone
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK
$ test ! -e docs/testing-dag.md && ! grep -q 'Orthogonal DAG planner' zensical.toml && echo page-gone
page-gone
$ ! grep -rn 'DagPlanner\|FsDigestCache\|FsMerkleHasher\|SupportRegistry\|category_label\|evidence_json' crates/slipfork/src && echo cli-planner-free
cli-planner-free
$ cargo run -q -p slipfork -- --help | grep -q -- '--verbose' && echo verbose-flag
verbose-flag
$ cargo test -p slipfork --lib logs
... test result: ok
$ cargo test -p slipfork-dag --test critical_dag
... test result: ok
$ cargo test -p slipfork-dag --test critical_support
... test result: ok
$ ! grep -rn 'getcwd() failed' crates/ --include=*.rs && echo no-getcwd-noise
no-getcwd-noise
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ make check
... exit 0
```

- `dag-gone`, `page-gone`, `cli-planner-free` and `check_docs_honesty: OK` are the four proofs of the deletion sweep; `critical_dag` and `critical_support` must still pass **unchanged** (the planner/support machinery is retained for `0.2.0`, not swept).
- `make check` exits 0 on a non-Btrfs dev host; world-entry assertions are the live node's job.
