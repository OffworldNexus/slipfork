# HUMAN inbox

Requests for the project owner. Agents add typed items with `uv run --no-project tools/plan.py --agent <role> human add --kind <kind> "..."`.

To answer, write under **Answer:**, save, and run `/human` (or tell `conductor` "answered"). Never paste raw secrets here.

## H-001 [closed] [direction-challenge] (node M1.5)

- asked: 2026-11-14T22:30 by architect

Local component commands (`snow.yml` vs `slipfork.toml`): In `snow.yml`, production components often omit `run_command` (relying on `Dockerfile` CMD) or invoke `modelw-docker run ...`. Option A (recommended): allow optional per-mode component overrides (`modes.local.components.<id>.run_command`) in `snow.yml` so `snow.yml` stays the single manifest for `slipfork up`, while keeping test-only `[artifacts]` & `[fixtures]` DAG nodes in optional `slipfork.toml`. Option B: keep `modes` strictly env-only in `snow.yml` and put local `run_command` overrides in `slipfork.toml`.

**Answer:**
Approved Option A: allow optional per-mode component overrides (`modes.local.components.<id>.run_command`) in `snow.yml`, keeping `[artifacts]` and `[fixtures]` test-DAG configuration in optional `slipfork.toml`.

## H-002 [closed] [direction-challenge] (node M1.5)

- asked: 2026-11-14T22:30 by architect

Canonical `snow.yml` v2 syntax lock (`ADR-0001`): Confirm locking (1) `::` as the sole namespace separator (`${{ secrets::SECRET_KEY !! random_string(50) }}`), (2) unquoted `all:` as the sole universal mode key inside `modes:` (rejecting `*` which is a YAML alias token), and (3) strict mutual exclusion between top-level `envs:` and `modes:` (with `slipfork init` migrating legacy `envs:` files).

**Answer:**
Approved: lock `::` separator, unquoted `all:` universal mode, and strict mutual exclusion between `envs:` and `modes:`.

## H-003 [closed] [direction-challenge] (node M1.5)

- asked: 2026-11-14T22:30 by architect

`!!` secret persistence sink in `local` mode (`ADR-0001`): Confirm `LayeredSecretProvider` behavior for `local` mode — reads `LocalVault (~/.local/share/slipfork/vaults/<project>.json, 0600) -> Remote OSPP/Kerfufoo`, and persists `!!`-generated secrets (`SECRET_KEY`) into `LocalVault` so laptops work offline without write access to shared cloud secrets, while `cloud` mode in Kerfufoo persists `!!` directly into `EnvGroup`'s encrypted `Secret` store.

**Answer:**
Approved: `LayeredSecretProvider` persists `!!`-generated secrets to the local `0600` vault in `local` mode and to Kerfufoo `EnvGroup` secrets in `cloud` mode.

## H-004 [closed] [missing-input] (node M2)

- asked: 2026-11-14T22:30 by architect

Host `incusd` exposure for M2 live hypervisor integration (`ADR-0002`): This OpenCode session runs inside an unprivileged LXC/Incus guest (`/dev/mapper/chulak-incus` XFS with working `FICLONE` + `unshare` `netns`, guest `/dev/incus/sock` only, no `/var/lib/incus/unix.socket` or `/dev/kvm`). M1 proved `ReflinkNetnsEngine` live and tested `IncusEngine` against a Unix-socket REST harness. For M2 live `IncusEngine` (LXC + QEMU stateful RAM snapshot) tests, should we mount a dedicated project-restricted Incus Unix/HTTPS endpoint into this container, or run nested `incusd`?

**Answer:**
SUPERSEDED BY OWNER RULING (2026-11-16): Incus is permanently deleted from Slipfork. Option A (unified Btrfs subvolumes + Linux namespaces / `crun`, `BtrfsEngine`) is the sole `WorldEngine` with zero backward compatibility and zero fallbacks.

## H-005 [closed] [missing-input] (node M3.16)

- asked: 2026-11-17T00:40 by lead

DESTRUCTIVE-ACTION AUTHORIZATION NEEDED: DO volume 131a2765-e1ba-11f1-b67b-d6114774f516 (slipfork-dev-vm, ams3, 100 GiB) is NOT Btrfs as believed — blkid reports GPT+PMBR with /dev/sda1 TYPE="zfs_member" LABEL="incuspool" (the deleted M3.5 Incus/ZFS substrate). M3.16's fail-closed rule forbids formatting it without your word. Droplet 619867764 was recreated then destroyed by the harness's down --yes; the volume is now unattached and no droplet exists. Options: (A) RECOMMENDED — authorize `wipefs -a` + `mkfs.btrfs -f` on this tagged dev volume (its only content is the obsolete Incus ZFS pool; the engine is deleted by M3.21) so do_vm.py up provisions it and M3.19 can run live; (B) create a fresh 100 GiB Btrfs volume in ams3 and retag it slipfork-dev-vm, leaving the ZFS volume for you to delete (extra ~$10/mo until removed). Reply A or B (or say 'wipe the volume').

**Answer:**

## H-006 [closed] [direction-challenge] (node M3.19)

- asked: 2026-11-17T12:45 by lead

Live VM blocks on AppArmor: every world boot dies at the first crun backing service. Ubuntu 24.04 transitions the in-world /.slipfork/bin/crun into the unprivileged_userns domain on execve, so userns_create is DENIED and postgres:18 exits with 'unshare: Operation not permitted'. ADR-0010 §7's claim that unprofiled children stay under the slipfork profile across execve is false, and the shipped profile (apparmor.rs + tools/do_vm.py) attaches only <bin_dir>/slipfork{,d}. Options: (A, recommended) extend the profile attachment set to the whole world launcher stack — <bin_dir>/slipfork{,d}, <bin_dir>/slipfork-world-init (and the cargo target path used by the harness), /usr/bin/unshare and /.slipfork/bin/crun — and correct ADR-0010 §7 to describe the real mechanism (the profile must name every executable that calls unshare(CLONE_NEWUSER), not just the entry point); (B) stage crun at one stable host path and attach the profile there (does not remove the need to name unshare/world-init); (C) drop the OCI user namespace for worlds (changes the isolation model). Consequence today: slipfork engine+CLI live suites, the benchmark, and the /home/slipfork/demo flow on 165.232.86.174 cannot boot a world until the profile is extended; the droplet is UP and the demo repo is staged. Recommendation: A. This blocks the 0.1.0 live-verification claim (M3.19).

**Answer:**

## H-007 [closed] [direction-challenge] (node M3.29.3)

- asked: 2026-11-18T03:17 by lead

OSPP endpoint has no durable source of truth in 0.1.0. 'slipforkd --ospp-endpoint' works only for a hand-run foreground daemon: (a) the auto-spawned daemon (up/fork) never receives it, (b) 'daemon unit'/'daemon install' render no Environment= or config key for it, (c) there is no 'daemon stop', so pairing cannot survive a reboot. Consequence: 'slipfork auth' + live subscriptions only work with a manually started slipforkd. Decide ONE: (A) SLIPFORK_OSPP_ENDPOINT read in the composition root only, embedded into the unit by 'daemon install' (Environment=), documented as the single override; (B) a [ospp] endpoint key in slipfork.toml, resolved by slipforkd at boot (needs the config schema + docs); (C) keep 0.1.0 pairing-only-by-hand and publish that constraint. Recommendation: A (no schema change, one ambient read at the composition root, and 'daemon install' already owns the unit bytes).

**Answer:**

## H-008 [closed] [release-gate] (node M3)

- asked: 2026-11-19T07:17 by conductor

Review & merge Release 0.1.0 PR (https://github.com/OffworldNexus/slipfork/pull/2, superseding #1) into develop; live demo running on http://demo.localhost (:80) on 165.232.86.174.

**Answer:**

## H-009 [closed] [release-gate] (node M3)

- asked: 2026-11-22T09:43 by conductor

Slipfork 0.1.0 clean-sheet release ready on release/0.1.0 -> develop (PR #2): pure-Btrfs native OCI/Dockerfile engine, clap + frozen-pane indicatif TUI, native hyper Btrfs S3, ~0.8s full-stack fork, visual Zensical docs, all-distro Linux packaging (.deb/.rpm/Arch/Alpine/portable + install.sh + multi-platform slipfork-lsp), and live demo verified on 165.232.86.174.

**Answer:**

## H-010 [closed] [release-gate] (node M3)

- asked: 2026-11-22T11:15 by conductor

Slipfork 0.1.0 clean-sheet release ready on release/0.1.0 -> develop (PR #2): pure-Btrfs native OCI/Dockerfile engine, clap + frozen-pane indicatif TUI, native hyper Btrfs S3, ~0.8s full-stack fork, visual Zensical docs, all-distro Linux packaging (glibc<=2.34 .deb/.rpm/Arch/Alpine/portable + install.sh + multi-platform slipfork-lsp), live demo on 165.232.86.174, and full GHA CI + Docs + Packages.

**Answer:**

## H-011 [closed] [direction-challenge] (node M3.50.6)

- asked: 2026-11-22T16:36 by lead

M3.50 live demo fork median is 1.10s (5 warm samples 1.22/1.10/1.10/1.11/0.98) on the 4-vCPU droplet; ceiling is <1.0s. Root cause is cold concurrent crun container startup for the 8 demo pods (pg step 542-825ms; warm 'slipfork restart pg' is 42ms; PG recovery 0ms; snapshot 145-249ms incl. one syncfs). The fresh-store lab median is 892ms. Options: A) accept 1.10s on this droplet, document the environment bound, keep 892ms lab median as the contract number; B) drop the per-fork syncfs (~100ms) and accept that writes after 'up' are not guaranteed durable in a fork; C) pre-warm crun/containers or provision a faster VM for the acceptance run. Recommendation: A, with B as a follow-up knob.

**Answer:**

## H-012 [closed] [release-gate] (node M3.51)

- asked: 2026-11-22T18:25 by lead

Pre-existing live defect outside M3.51 scope, reproducible on 165.232.86.174: critical_fork_live's big journey (spawn->fork benchmark section) sees the parent INSERT row before fork but the fork's app_db has 3 rows instead of 4 (last committed row missing in the fork snapshot) despite the M3.50.6 syncfs call; the extracted M3.51.2 rollback journey passes. Also resolved a spec blind spot in favor of specs/01 §4: 'cdn <id>' is cloud-only, so tests now use assets.<world>.localhost and assert static-cdn.<world>.localhost is 404; specs/03 §server table still mentions CDN virtual-host dispatch and should be reconciled. M3.51 gate itself is green.

**Answer:**

## H-013 [closed] [release-gate] (node M3.52)

- asked: 2026-11-22T19:13 by lead

Two pre-existing protected-test regressions block a 100% live gate, both outside M3.52: (1) critical_world_journey's failed-service tail prints only 'Failed to start service ...' with no 'error:' / Forkfile:line / next: (regression from M3.50.4 failure_sentence swallowing the Fault detail); (2) critical_fork_live's mismatched-epoch assertion demands frontend state 'built', but a warm content-addressed image cache returns 'cached' since the CDN arg no longer changes the digest. M3.52 itself is proven: critical_fork_live passed 4/4 rows + pre-fork checkpointer delta + 1.016s median before (2); world_journey passed every earlier assertion after test-path repairs. Also note droplet fork medians oscillate 0.98-2.0s (H-011 host bound).

**Answer:**

## H-014 [closed] [release-gate] (node M3)

- asked: 2026-11-22T19:51 by conductor

Slipfork 0.1.0 ready on release/0.1.0 -> develop (PR #2): pure-Btrfs native OCI/Dockerfile engine, DECSTBM bottom-bar 1-line-per-task TUI (2D VT100 verified), unified PTY slipfork shell, domain-first ps tree + .forks/* detection, indexed timestamped logs, <5ms wire PG CHECKPOINT, offworld-packaging (stage-dist.py + *.offworld-nexus.app, zero Cloudsmith/Forgejo), and 8/8 live suites green on 165.232.86.174.

**Answer:**

## H-015 [closed] [release-gate] (node M3)

- asked: 2026-11-22T23:37 by conductor

Slipfork 0.1.0 ready on release/0.1.0 -> develop (PR #2): pure-Btrfs engine, slirp4netns world egress, DECSTBM + t=0 pre-sized ui::table concurrent spinners, per-step #run_id logs (-r/--runs), declared domain <id> grocery-list routing, root-down ps fork visibility, real uv+FastAPI & pnpm+SvelteKit demo live on 165.232.86.174, and offworld-packaging.

**Answer:**

## H-016 [closed] [release-gate] (node M3)

- asked: 2026-11-22T23:49 by conductor

Slipfork 0.1.0 ready on release/0.1.0 -> develop (PR #2): pure-Btrfs engine, slirp4netns world egress, DECSTBM + t=0 pre-sized ui::table concurrent spinners, per-step #run_id logs (-r/--runs), declared domain <id> grocery-list routing, root-down ps fork visibility, real uv+FastAPI & pnpm+SvelteKit demo live on 165.232.86.174, and offworld-packaging.

**Answer:**

## H-017 [closed] [direction-challenge] (node M3.60)

- asked: 2026-11-23T00:24 by lead

M3.60 test alignment is proven green on the VM (critical_world_journey 2/2 in 11.5s; critical_status_live 5/5 in 20.4s under the make live-test contract), but critical_fork_live fails its protected H-011 gate: 8-component fork spawn->first-200 median 6.272s (6.090/6.272/6.318) vs the 1.25s ceiling, while the CoW snapshot itself is 151-289ms. Cause is the M3.55 FastAPI/SvelteKit demo boot readiness (SvelteKit vite dev + uvicorn first 200), outside M3.60's crates/slipfork/tests/critical_*.rs paths. Options: A (recommended) open a follow-up M3 task to make the demo fork-ready fast (prebuilt SvelteKit prod build instead of vite dev, parallel readiness), then rerun fork_live; B renegotiate the H-011 ceiling for the new 8-component demo composition. Raising the ceiling inside M3.60 would weaken a protected critical test, so I did not.

**Answer:**

## H-018 [closed] [direction-challenge] (node M3.60)

- asked: 2026-11-23T00:49 by lead

H-018: 5.0s end-to-end fork ceiling is not reachable with the REAL FastAPI uvicorn --reload + SvelteKit vite dev demo. Direct-bin launch is proven (argv /project/.venv/bin/uvicorn, /project/front/node_modules/.bin/vite; wrapper eliminated) and snapshots are 148-365ms. Clean VM (4-vCPU, demo world stopped): samples 5.241/5.518/6.226 (median 5.518); another clean run 5.412/5.607/5.640 (median 5.607); uvicorn --reload-dir app does not help. Split: fork --json ~3.66s (pg .63-.96, worker .68, mailpit .5, front 2.7, api 3.2-3.5) + first-200 ~1.9s (vite SSR + api). 5.0s will also fail on 2-vCPU CI. Options: A (recommended) set the honest 8-component end-to-end median ceiling to 7.0s for 4-vCPU VM / 2-vCPU CI while keeping the owned SLOs (snapshot_ms <500ms, zero image rebuilds, zero uv sync/pnpm install/pre-deploy re-runs) strict; B open a follow-up to trim the api/front boot tail (prod SvelteKit build or no --reload), which conflicts with keeping the exact dev stack; C revert to toy scripts (rejected). Not weakening the protected test inside M3.60 without this decision.

**Answer:**

## H-019 [closed] [release-gate] (node M3)

- asked: 2026-11-23T02:34 by conductor

Slipfork 0.1.0 ready on release/0.1.0 -> develop (PR #2): pure-Btrfs engine, slirp4netns egress, DECSTBM + t=0 pre-sized ui::table concurrent spinners, #run_id logs (-r/--runs), on/else + domain.name vs self.{url,domain,scheme} + "${...}" + secret/var(KEY, gen), Tree-sitter/Zed/Vim bundled in all Linux packages, real uv+FastAPI & pnpm+SvelteKit demo live on 165.232.86.174, and offworld-packaging.

**Answer:**

## H-020 [closed] [release-gate] (node M3)

- asked: 2026-11-23T02:41 by conductor

Slipfork 0.1.0 ready on release/0.1.0 -> develop (PR #2): pure-Btrfs engine, slirp4netns egress, DECSTBM + t=0 pre-sized ui::table concurrent spinners, #run_id logs (-r/--runs), on/else + domain.name vs self.{url,domain,scheme} + "${...}" + secret/var(KEY, gen), Tree-sitter/Zed/Vim bundled in all Linux packages, real uv+FastAPI & pnpm+SvelteKit demo live on 165.232.86.174, and offworld-packaging.

**Answer:**

