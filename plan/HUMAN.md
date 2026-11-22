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

## H-011 [open] [direction-challenge] (node M3.50.6)

- asked: 2026-11-22T16:36 by lead

M3.50 live demo fork median is 1.10s (5 warm samples 1.22/1.10/1.10/1.11/0.98) on the 4-vCPU droplet; ceiling is <1.0s. Root cause is cold concurrent crun container startup for the 8 demo pods (pg step 542-825ms; warm 'slipfork restart pg' is 42ms; PG recovery 0ms; snapshot 145-249ms incl. one syncfs). The fresh-store lab median is 892ms. Options: A) accept 1.10s on this droplet, document the environment bound, keep 892ms lab median as the contract number; B) drop the per-fork syncfs (~100ms) and accept that writes after 'up' are not guaranteed durable in a fork; C) pre-warm crun/containers or provision a faster VM for the acceptance run. Recommendation: A, with B as a follow-up knob.

**Answer:**

