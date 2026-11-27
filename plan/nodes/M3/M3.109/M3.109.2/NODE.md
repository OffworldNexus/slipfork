---
id: M3.109.2
title: "slipfork docker namespace: login/logout/list/build/pull/images with live build events"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.109.1]
paths:
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/compose.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/surface.rs
  - "crates/slipfork/src/world/**"
  - "crates/slipfork/tests/**"
  - "packaging/man/**"
  - "specs/02-cli-ux.md"
  - "specs/03-under-the-hood.md"
  - docs/cli.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T15:59"
updated: "2026-11-24T16:18"
---

# Goal

Ship `slipfork docker login|logout|list|build|pull|images` on top of M3.109.1's injected credential store, with `--json` purity and one `next:` on failure. It is a Docker-like surface, never a daemon claim.

# Pre-baked context

- `crates/slipfork/src/cli/verbs.rs:63` `Command` enum and `:290` `SecretCommand`; add `Docker { #[command(subcommand)] command: DockerCommand }` + a `DockerCommand` tree. `crates/slipfork/src/cli/dispatch.rs:44` `dispatch` match; add the arm.
- `crates/slipfork/src/commands/mod.rs:40` `Session` (cwd/terminal/host); `:258` `report` renders `{"error":{code,message,next}}` / `error:`+`next:`. `Session::stdin_is_terminal()` at `:111`; `emit` at `:318`.
- Existing stdin secret reader: `crates/slipfork/src/commands/secret.rs` (`secret set` never echoes).
- Build wiring reference: `crates/slipfork/src/world/runtime/images.rs:198-348` (`build_uncached`): parse `ImageReference`, `OciPuller::pull_into`, `BuildRequest{context,dockerfile,args,target,externals}`, `pull_externals`, `NativeImageBuilder::build` -> `BuildReport{rootfs,key,config,...}`. `crates/slipfork-image/src/builder/mod.rs:35`; `:80` `stage_walk`.
- Forkfile resolution: `slipfork_forkfile::compile`, `world_plan_with(&doc, project, &features)` gives `plan.images` (`ImageNode{id,from,directory,dockerfile}`); `crates/slipfork/src/world/mod.rs` `HostEnv` has store/arch/crun/uid/gid/resolv_conf plus M3.109.1 `registry_credentials`.
- Live frames: `crates/slipfork/src/world/runtime/images/log.rs` `BuildLog` + `crates/slipfork/src/world/supervise.rs:392` `BootSink` (pub(super)); `slipfork_image::StepLog::with_events` emits `BuildEvent`/`StepEvent`. Show the same `[i/n]` step rows as `up`; `--json` emits NDJSON frames with zero ANSI.

# Change

1. Verbs: `docker login [HOST]` (`--username`, `--password-stdin`; default `docker.io`; TTY no-echo prompt; never argv), `logout [HOST]`, `list`, `build <component> [--target <stage>]`, `pull <ref>`, `images`; every verb `--json`.
2. `commands/docker.rs` keeps verbs thin: login verifies through the M3.109.1 probe then saves to the 0600 store; logout removes; list prints host + username (dynamic widths, never secret); images lists cached OCI refs/digests from `ImageLayout` via injected store root.
3. `build` resolves the component from `./Forkfile`, uses the **existing** `OciPuller` + `NativeImageBuilder`/step cache (no second pipeline), streams live step events, prints the digest; `pull` uses `OciPuller`; both fail as one line + `next:`.
4. Wire `HostEnv.registry_credentials` into image builds (M3.109.1) so `docker pull` and private `FROM`/`COPY --from` use the store.
5. Docs: `docs/cli.md` daily-verbs row + a short `## Docker-compatible registry verbs` section (copy-pasteable, no daemon claims); `specs/02-cli-ux.md` verb contract; `specs/03` port map notes `OciPuller` receives injected `RegistryCredentials`.

# Acceptance

- `make check` passes; `make docs-check`; no ANSI under `NO_COLOR`/`--json`.
- `docker list` prints `HOST USERNAME` only; wrong password -> one line + one `next:`; no secret in stdout/stderr/json.
- No `slipfork docker` claim in docs beyond the shipped verbs.
