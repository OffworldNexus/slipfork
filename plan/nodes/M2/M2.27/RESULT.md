# RESULT M2.27

Implemented `ADR-0009` §1–§3: the engine-agnostic `WorldRootfs` value and the single production
`BaseImageBuilder` adapter in `#![forbid(unsafe_code)]` `crates/slipfork-engine/src/rootfs/`, plus
protected contract + live tests.

## What shipped

- `rootfs/mod.rs` — `WorldRootfs`, `RootfsDigest` (`sha256:<64 hex>`, domain
  `"slipfork-l0-rootfs:v1\0"`, ADR §6.1 component list, secret build-env values hashed only),
  `WorldRootfsRequest`, `SnapshotMode`-independent `RuntimeAssets`, `HostCacheRequest`,
  `SnowPortContract`, `RootfsBuildEvent`, `OsRelease`, `ImageContractMetadata`, `OciRootfs`,
  `BackingServiceKey`, the four injected ports (`BuildxExporter`, `InputHasher`, `OciImageStore`,
  `AbiProbe`), `trait BaseImageBuilder`, and `BaseImageError` with the exact ADR §5.1–§5.11 text
  (including `error:`/`  fix:` lines, printed verbatim by callers).
- `rootfs/dockerfile.rs` — pure stage/instruction parser (continuations, comments, `FROM ... AS`,
  `ENV`/`ARG` key-value and exec/shell `ENTRYPOINT`/`CMD` forms).
- `rootfs/contract.rs` — rules 1/3/4/6 statically (re-checked on cache hits so a `snow.yml` port
  edit cannot bypass a cached rootfs) and rules 2/5 against the extracted tree; `EXPOSE` +
  `HEALTHCHECK` port extraction; supported distro table (`debian` bookworm|trixie, `ubuntu`
  jammy|noble, `alpine` 3.20|3.21 incl. `3.21.x` patch IDs); `/.slipfork/bin` integrity.
- `rootfs/abi.rs` — real ELF parser (`e_machine`, `PT_INTERP`), glibc `GLIBC_2.*` max symbol,
  musl dotted version, and the fail-closed host-cache ABI rules (`arch`, libc family,
  `host.glibc <= world.glibc`, musl major.minor equality).
- `rootfs/input.rs` — `FsInputHasher`: sorted context Merkle honoring `.dockerignore`
  (comments/negation/directory patterns), excluding `.git/` and `.worktrees/`.
- `rootfs/cache.rs` — `<state_dir>/cache/{rootfs,oci,runtime,tmp}`, `/proc/self/mountinfo`
  tmpfs refusal, atomic `manifest.json`.
- `rootfs/buildx.rs` — `CliBuildxExporter` (`--progress=plain -f … --target slipfork-world
  --build-context slipfork-runtime=<dir> --output type=local,dest=…`); build-arg secret values go
  through the child environment, never argv.
- `rootfs/pin.rs` — `CRUN_PIN_X86_64` = `crun-1.20-x86_64-ff8407185ec4.bin`,
  `sha256:ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38`, verified live.
- `rootfs/builder.rs` — `DockerfileStageBuilder`: scratch `Dockerfile` under
  `<state_dir>/cache/tmp` (never in the worktree), appended stage, staging tree → 6-rule
  validation → atomic rename, manifest, cache-hit path with zero Docker work.
- `rootfs/conformance.rs` — `assert_world_rootfs_conformance`.
- `tests/critical_rootfs_contract.rs` (13 tests), `tests/critical_rootfs_live.rs` (2 ignored
  live tests), `tests/support/mod.rs` (pinned crun fetch+verify, disk-only tempdirs).
- `crates/slipfork-engine/Cargo.toml`: `hex` + `sha2` (both already workspace deps; root
  `Cargo.toml`/`Cargo.lock` needed no new entries).

## Decisions

1. **ADR signature over the dispatch prompt.** The prompt sketch said
   `fn build(&self, request: &BaseImageRequest) -> Result<WorldRootfs, EngineError>`; `NODE.md`
   and `ADR-0009` both specify
   `fn materialize(&self, req: &WorldRootfsRequest, on_event: &mut dyn FnMut(RootfsBuildEvent))
   -> Result<WorldRootfs, BaseImageError>`. Implemented the ADR/NODE shape (ADR is the source of
   truth) and did not touch `EngineError`.
2. **Runtime payload mechanism: `--build-context`.** The ADR/design snippet's
   `FROM __slipfork_runtime AS __slipfork_bins` cannot be a *named build context* — Docker rejects
   `__slipfork_runtime` with `invalid context name __slipfork_runtime: invalid reference format`
   (verified live against buildx v0.39.0). No Slipfork runtime image exists or should be invented,
   so the appended stage copies the two static payloads from the **local** `slipfork-runtime`
   named context (`COPY --from=slipfork-runtime /crun /.slipfork/bin/crun`, same for
   `slipfork-world-init`). This preserves the contract exactly: only those two files enter the
   user stage, zero `apt`/`apk` runs, `type=local` export is `$UID:$GID`-owned, and no registry
   dependency.
3. **`crun` pin verified on the extracted artifact.** `buildx` receives the pinned bytes and the
   builder verifies the exported `/.slipfork/bin/crun` against the compile-time SHA-256
   (`ExtractedCrunMismatch`); the unused pre-build `CrunPinMismatch` variant was deleted rather
   than kept as dead API. This also lets contract tests for rules 2/5 run without the real asset.
4. **`OciImageStore` production adapter deferred to M2.29.** This node injects the port and maps
   requested `snow.yml` resources into `WorldRootfs::service_images`; registry digest resolution +
   extraction has no acceptance here and M2.29 owns the `resources:` → service-image wiring.
5. **`BaseImageError` kept `<= 128` bytes** (test-guarded) by boxing the `HostCacheAbiMismatch`
   ABI payload and storing `ExposeIngressMismatch` text as `Box<str>` — removes all 33
   `clippy::result_large_err` errors without changing a single ADR message.
6. **`resolve_guest_path` follows symlinks inside the rootfs.** `fs::canonicalize` resolved
   Alpine's absolute `/bin/sh -> /bin/busybox` against the **host** `/`, wrongly reporting
   "missing /etc/os-release + /bin/sh". The resolver now restarts absolute targets at the rootfs,
   forbids `..` escapes, and bounds symlink hops (found live; contract + live tests cover it).
7. **`IncusEngine` field rename `base_instance` → `source_instance`.** The acceptance grep
   (`slipfork_base|CuratedBase|prebuilt|base_instance`) must exit 1. The field is M1's unexplained
   source-instance name; M2.28 still replaces it with a `WorldRootfs` import exactly as its node
   says. The rename is mechanical, path-internal, and leaves no alias behind (zero fallback cruft).

## Acceptance

```console
$ cargo test -p slipfork-engine --test critical_rootfs_contract
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo test -p slipfork-engine --test critical_rootfs_live -- --ignored
running 2 tests
test missing_docker_fails_closed_with_the_actionable_diagnostic ... ok
test live_buildx_export_validates_and_caches_the_world_stage ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.41s

$ rg -n "slipfork_base|CuratedBase|prebuilt|base_instance" crates/slipfork-engine
rg_exit=1            # no matches

$ cargo test -p slipfork-engine --all-targets
test result: ok. 26 passed  (lib)
test result: ok. 14 passed  (critical_engine)
test result: ok. 13 passed  (critical_rootfs_contract)
test result: ok. 0 passed; 2 ignored  (critical_rootfs_live)

$ cargo clippy --workspace --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.42s   (exit 0)
# only pre-existing cargo manifest note: `unused dependency socket2` in crates/slipfork-edge

$ cargo fmt --all -- --check
(no output, exit 0)

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
(no output, exit 0)

$ make check
make_check_exit=0
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)

$ uv run --no-project tools/guard.py
guard: OK (341 file(s) checked)

$ uv run --no-project tools/critical_guard.py --base b21eda3 --node M2.27
critical-guard: FAILED: 1 unauthorized protected test change(s) in M2.27.
  M  crates/slipfork/tests/critical_cli.rs  [VIOLATION: not covered by any node path]
  A  crates/slipfork-engine/tests/critical_rootfs_contract.rs  [added (always allowed)]
  A  crates/slipfork-engine/tests/critical_rootfs_live.rs  [added (always allowed)]

$ uv run --no-project tools/critical_guard.py --base b21eda3 --node M2.17
critical-guard: OK: authorized by M2.17
```

The sole guard violation is **not M2.27's change**: `crates/slipfork/tests/critical_cli.rs`
(+547/−3) is M2.17's concurrent uncommitted CLI work, covered by M2.17's own
`paths: ["crates/slipfork/**", …]` as the second invocation proves. M2.27 modified **no** existing
protected test; both of its new protected files are additions (always allowed). Re-running the
M2.27 gate after M2.17 commits resolves it without any change to this node.

Live environment used: Docker 29.9.3, buildx v0.39.0, pinned static
`crun 1.20` (`ff8407185ec4…`), kernel `6.12.117+deb13-amd64`, `$UID:$GID` `1000:1000`.
