# RESULT M3.23 — Docs hygiene & 0.1.0 version gate

- **Status:** done
- **Attempt:** 1/3 (base `8417868`)
- **Branch:** `release/0.1.0`

## 1. Version single-source (`0.1.0`)

| Surface | Before | After |
|---|---|---|
| `Cargo.toml` `[workspace.package].version` | `0.2.0` | `0.1.0` |
| `crates/*/Cargo.toml` (10 members) | `version.workspace = true` | unchanged — all inherit `0.1.0`; no member pins its own version |
| `Cargo.lock` (10 `slipfork*` packages) | `0.2.0` | `0.1.0` (regenerated via `cargo metadata`; only `nohash-hasher` remains at a third-party `0.2.0`) |
| `packages/opencode-slipfork/package.json` | `0.2.0` | `0.1.0` |
| `zensical.toml` nav | `CLI (0.2.0)` | `CLI (0.1.0)` |
| `CHANGELOG.md` | `0.2.0` section + old pre-pivot `0.1.0` section | **one** `## 0.1.0 (2026-11-17)` entry describing the shipped Btrfs Worlds surface; `0.2.0`/`0.3.0` headings deleted |
| Published page titles | `docs/snow-modes.md:1`, `docs/reference/manifest.md:1`, `docs/agents-and-opencode.md:1` = `0.2.0` | `0.1.0` |
| `ARCHITECTURE.md` shipped labels | `:15`, `:95`, `:98`, `:607` = `shipped in 0.2.0` | `shipped in 0.1.0` |
| ADR status lines | `ADR-0010:3` / `ADR-0002:3` = `M3 (0.3.0)` | `M3 (0.1.0)` |
| `docs/VISION.md` | `shipped in 0.2.0`, `M2 (0.2.0)`, `M3 (0.3.0)`, `M4 (0.4.0)` | `shipped in 0.1.0`, `M2 (0.1.0)`, `M3 (0.1.0, STP v1 in 0.2.0)`, `M4 (later)`; STP v1 prose moved to `0.2.0` |
| `docs/design/world-base-image.md:602` | `shipped in 0.2.0` | `shipped in 0.1.0` |
| `crates/slipfork-vcs/src/remote_helper.rs:42` | `in 0.2.0` | `in 0.1.0` |
| `packages/.../tests/fixtures/cli-contract.json:2` | `slipfork 0.2.0 (release/0.2.0, e5ac58f)` | `slipfork --help (commit e5ac58f)` (no version claim) |

Future labels kept deliberately: `docs/testing-dag.md:80-81`, `README.md:59`, `docs/index.md:64` (`0.2.0` = parked STP v1), and `daemon.rs:315` (typed `WorldId` is `0.2.0` target state).

## 2. Terminology gate (owner rule)

`! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/` → **exit 0** (raw grep exit 1, no matches). Stale `tools/**/__pycache__` binaries (gitignored) were removed; `make check` regenerates them from the cleaned sources.

- Deleted `docs/design/adr-0008.review-cruft.md` and `docs/design/adr-0008.review-ioc-upward.md` (adjudicated artifacts carrying `Option A`/`Option B`).
- `tools/tests/test_tools.py`: fixture `"Use Option A."` → `"Use local mode."`; same `HUMAN.md` parser branch covered.

## 3. Published-doc defects fixed (before → after)

| Location | Before | After |
|---|---|---|
| `docs/reference/manifest.md:16` | `run_command: … --bind 0.0.0.0:${{ components::api.private_port }}` | `--bind 0.0.0.0:8000` (matches `internal_ports: [8000]`) |
| `docs/reference/manifest.md:44` | `--port ${{ components::api.private_port }} --reload` | `--port 8000 --reload` |
| `docs/reference/manifest.md:51` | (nothing) | one sentence: `run_command` is passed verbatim to `sh -eu -c`, **not** expression-templated; name the literal port you declared |
| `docs/snow-modes.md:17,51` | same `${{ components::api.private_port }}` inside `run_command` | literal `8000` |
| `docs/snow-modes.md:58` | (nothing) | same non-templating sentence |
| `ARCHITECTURE.md:101` | `shared project::resolve_world_selector -> WorldId` | shipped private `daemon::resolve_world_selector -> WorldTarget`; typed `WorldId` + `project::` is `0.2.0` target state |
| `docs/design/target-dx.md:115` | same `project::resolve_world_selector` claim | same shipped/target split |
| `docs/decisions/ADR-0008…:53` | same `project::resolve_world_selector` / `WorldId` claim | shipped `daemon::resolve_world_selector -> WorldTarget`; typed route is `0.2.0` |
| `docs/design/target-dx.md:116` | “`<fork>` equal to the project name is rejected” | `<project>:<project>` accepted (colon disambiguates); bare `<fork> == project` rejected only when creating via `slipfork fork` |
| `docs/snow-modes.md:1,124` | `0.2.0` | `0.1.0` |
| `docs/reference/manifest.md:1,159-160` | `0.2.0`; “lands in Milestone 3” | `0.1.0`; “not shipped in `0.1.0`; lands with `STP v1` in `0.2.0`” |
| `docs/agents-and-opencode.md:1` | `0.2.0` | `0.1.0` |
| `crates/slipfork/src/cli.rs:108-116` | “Preview or reap orphaned worlds and snapshots” | “Preview (default) or reap worlds whose checkout directory is gone. Sealed snapshot bases and the rootfs/OCI caches are retained, not collected.” |
| `docs/reference/cli.md:55` | “Preview orphaned worlds by default; `--yes` reaps them” | names the checkout-gone condition, `--dry-run`, and the retained snapshots/caches |
| `docs/getting-started.md:156-162` | profile-only AppArmor remediation | adds: the profile must name **every** `unshare(CLONE_NEWUSER)` caller, and the dedicated-VM `kernel.apparmor_restrict_unprivileged_userns=0` alternative via `/etc/sysctl.d` |

**`prune --dry-run` decision:** **kept and documented truthfully.** The protected critical suite (`crates/slipfork/tests/critical_cli.rs:103-104`) pins `slipfork prune --dry-run` as a shipped contract, so per the node's escape clause the flag stays; `cli.rs` help, `docs/reference/cli.md`, and `docs/design/target-dx.md:131` now describe it as the explicit spelling of the default preview (mutually exclusive with `--yes`). The first `make check` run failed exactly here (`unexpected argument '--dry-run'`), which is the evidence for the decision.

## 4. Stale post-pivot residue removed

Deleted (adjudicated critic / pre-pivot design artifacts, 9 files):
`docs/design/m1-core-and-poc.md` (ReflinkNetnsEngine/IncusEngine/VolumeCloner/RunnerProtocol-NDJSON design),
`docs/design/adr-0008.review-{cruft,ioc-upward,legality}.md`,
`docs/design/adr-0008.review-{ai-ergonomics,dx}.md`,
`docs/design/m2-docs.review-{ai-ergonomics,dx}.md`,
`docs/design/m2-manifest-docs.review-dx.md`.

Rewritten in place:
- `docs/research/m1-poc-benchmark.md` — now labelled a **historical pre-`ADR-0009` baseline**; removed `ReflinkNetnsEngine`, `ioctl(FICLONE)`, Incus ZFS/VM-RAM snapshot claims; points at `btrfs-crun-benchmarks.md` for the shipped numbers.
- `docs/research/tunnel-weaver-and-systemd-facts.md:273` — deleted the `incus`/`incusd` probe row.
- `docs/knowledge/LESSONS.md:7` — `ioctl(FICLONE)`/`chulak-incus` → “reflink clone ioctl (`0x40049409`)”, generic XFS volume.
- `docs/design/world-base-image.md:611` — “no `SnapshotMode`” → “no snapshot-mode flags”.
- `docs/design/world-base-image.md:97,293,300` — dropped unshipped `--appendonly yes`; corrected the shipped S3 `ServiceSpec.id` to `s3_buckets` (`seaweedfs` is only the `BackingServiceKey` display).
- `docs/decisions/ADR-0010…:9` — Context §2 now cites `docs/research/btrfs-crun-benchmarks.md` as the sole owner of the figures instead of restating `19.5 ms` / `11–24 ms` / `39 ms` / `6.3 MiB`.
- `crates/slipfork-engine/src/rootfs/mod.rs:544` — `CacheOnTmpfs` fix line now names Btrfs with `-o user_subvol_rm_allowed,noatime`; no XFS/ext4 alternatives.
- `crates/slipfork-engine/src/rootfs/pin.rs:27` — doc pointer `§7.1` → `§8.1`.
- `crates/slipfork/tests/critical_slug.rs:714-733` — non-Btrfs branch no longer asserts an inverted “in-process daemon succeeds” contract; it skips the Btrfs-VM-only missing-`git` PATH isolation with an explicit message.
- `ARCHITECTURE.md` — declared the previously undeclared `WorldTarget`; `BaseSeedSpec.base_key_sha12: String` → typed `BaseKey`; `WorldHandle` custom `Debug` and `drop_fork(&WorldSelector)` already correct.

**Verified, not regressed:** `crates/slipfork-engine/src/rootfs/mod.rs:82` no longer says “Incus image name”; `crates/slipfork-engine/Cargo.toml` description already names `BtrfsEngine`/`SubvolumeDriver`.

## 5. Confidentiality & wire-format gates

- `tools/guard.py:12` — “learned from poc-orange” → “learned from an unrelated external audit”. No sibling/client repository name was added anywhere.
- `! grep -riE 'ndjson|jsonl|line-delimited' ARCHITECTURE.md README.md docs/ crates/ tools/` → **exit 0** (raw grep exit 1). STP v1 is CBOR-over-`SLIPFORK_PROTOCOL_FD` and 0.2.0-only.
- `! grep -rniE 'incus|ficlone|statefulram|snapshotmode' ARCHITECTURE.md README.md docs/ crates/ tools/ packages/ CHANGELOG.md` → **exit 0** (raw grep exit 1).
- `tools/tests/test_do_vm.py:1548` dropped the dead `incus` negative assertion; `tools/tests/test_run_benchmark.py:530` asserts the neutral `"reflink ioctl"` is absent instead of the banned token.

## 6. Acceptance

| Command | Exit |
|---|---|
| `! grep -riE 'option [ab]\|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/` | **0** (raw grep 1) |
| `! grep -riE 'ndjson\|jsonl\|line-delimited' …` | **0** (raw grep 1) |
| `! grep -rniE 'incus\|ficlone\|statefulram\|snapshotmode' … packages/ CHANGELOG.md` | **0** (raw grep 1) |
| `cargo run -q -p slipfork -- --version` | **0** → `slipfork 0.1.0` |
| `cargo build --workspace --locked` | **0** (lock not rewritten) |
| `uv run --no-project tools/plan.py validate` | **0** (`plan OK`) |
| `uv run --no-project tools/guard.py` | **0** (`OK (504 files)`) |
| `uv run --no-project tools/check_docs_honesty.py` | **0** (`OK (9 published docs)`) |
| `make docs-check` | **0** (`No issues found`) |
| `make check` | **0** (after the `prune --dry-run` revert; first run failed only on that protected contract) |

Release-record printouts:

```console
$ grep -n 'version' Cargo.toml packages/opencode-slipfork/package.json zensical.toml | head
Cargo.toml:17:version = "0.1.0"
Cargo.toml:21:rust-version = "1.94"
Cargo.toml:25:clap = { version = "4.5.28", features = ["derive"] }
Cargo.toml:33:serde = { version = "1.0.228", features = ["derive"] }
Cargo.toml:39:tokio = { version = "1", default-features = false, features = ["rt", "net", "time"] }
Cargo.toml:44:tungstenite = { version = "0.30.0", features = ["rustls-tls-webpki-roots"] }
packages/opencode-slipfork/package.json:3:  "version": "0.1.0",
```

```console
$ head -30 CHANGELOG.md
# Changelog

All notable changes to Slipfork are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0 (2026-11-17)

First public release: Btrfs copy-on-write worlds forked in milliseconds, a
daemon-served CLI, `snow.yml` mode/secret resolution, the orthogonal artifact &
fixture DAG planner, and OpenCode V2 worktree isolation.

### Added

- **`BtrfsEngine`** — the sole `WorldEngine`: one unified Btrfs subvolume per world (the `Dockerfile`-derived `L0` rootfs plus PostgreSQL, Redis, and S3 state), read-only snapshots, writable clones, and a fail-closed `KernelBtrfsDriver` probe that refuses any non-Btrfs state directory with `EngineError::BtrfsRequired { path, actual_fstype, mount_hint }` (`ADR-0010`).
- **`slipforkd`** — the sole stateful Composition Root and unprivileged daemon: TCP HTTP/WS ingress, a `0600` `daemon.sock`, a `0600` `http-ingress.sock` bridge for in-world `$BASE_URL`, inherited `LISTEN_FDS` activation, and explicit `--state-dir`/`--runtime-dir`/`--world-init`/`--crun` inputs.
- **Thin auto-spawning CLI** — `up`, `down`, `fork`, `drop`, `list`, `prune`, `secret set|unset|list`, `share`, `daemon unit`, `exec`, `shell`, `logs`, `hook run`, `db`, `redis`, `forward`, `auth login`, and `support` drive the daemon over `daemon.sock`; when nothing answers, the CLI starts `slipforkd --listen 127.0.0.1:0` in the background.
- **`Dockerfile`-derived `L0` world rootfs** — a `snow.yml` `BuildImage` stage is exported with `docker buildx build --output type=local`, cached content-addressed under `<state_dir>/cache/rootfs/<sha256>/app`, mounted read-only, validated against the World Image Contract, and executed as `$UID:$GID` (`ADR-0009`).
- **`[world]` manifest selector** — two optional keys in `slipfork.toml`: `world.image` (required when `snow.yml` declares several `BuildImage` entries, inferred for one) and `world.stage` (named `Dockerfile` stage, inferred when the final stage is named); unknown keys and unbuildable projects fail closed.
- **`--host-caches`** — per-invocation, ABI-gated read-only mounts of `~/.cargo`, `~/.rustup`, `~/.cache/uv`, `~/.local/share/uv`, and the pnpm store; never a `slipfork.toml` key.
- **Daemonless `crun` backing services** — `pg_clusters` → `postgres:<version>`, `redis_clusters` → `redis:<version>`, and non-empty `s3_buckets` → SeaweedFS run as OCI containers sharing the world's `127.0.0.1`; `slipfork db`/`redis` open `psql`/`redis-cli` inside them, and `hook run`/`logs`/`forward`/`shell` give interactive and retained-log access.
- **`git-remote-slipfork`** — a `slipfork://<fork>` Git remote that lists and fetches a running fork's branch and `refs/slipfork/<fork>/wip`.
- **`slipfork support`** and the generated **[integration support matrix](docs/support-matrix.md)** — ten integrations graded `declared` with verified recipe pages and protected critical tests, enforced by `tools/check_support.py`.
- **`slipfork auth login`** — OSPP device-key pairing from a `#handle=` URL, read from `argv` or stdin.
- **`LayeredSecretProvider`** — `slipforkd` resolves `secrets::`/`vars::` from the `0600` local vault first (the sole `!!` and `secret set|unset` write sink) and from a paired `OSPP v1` `env.bundle` second; an unreachable paired vault fails the render closed.
```

## 7. Follow-ups (not blocking 0.1.0)

1. **`poc/run_benchmark.py` output path (poc/** out of scope):** the harness writes its new Btrfs report to `docs/research/m1-poc-benchmark.md`, which is now the hand-maintained historical M1 baseline; `make bench` (opt-in) will report drift and `--write-report` would overwrite it. Reconcile the harness destination with `btrfs-crun-benchmarks.md`.
2. **Optional prose dedupe (LOG 19:19/22:27):** ADR-0009’s Composition Root mermaid still redraws `ARCHITECTURE.md` §1; ADR-0002 §1 restates the `BtrfsEngine::new` → `with_driver` sentence from ADR-0010 §5; ADR-0010 §3 and `ARCHITECTURE.md` §3.3 still overlap on the Phase-2 retention sets.
3. **`docs/design/target-dx.md:102`** example comment still says “Preview orphaned worlds” (harmless; could say “worlds whose checkout is gone”).
4. **Historical plan pointers:** completed M1 nodes still say “See `docs/design/m1-core-and-poc.md`”, now deleted (`plan/` out of scope).
5. **`docs/index.md:44-50`** restates the four cost-model numbers with a provenance pointer; if “report = sole owner of the numbers” is meant literally, convert that table to a link.
6. **`critical_slug.rs` non-Btrfs branch** now skips rather than asserting; the missing-`git` PATH isolation still runs only on the Btrfs VM (by design, per LOG 12:22 #3).
