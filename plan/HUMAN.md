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

## H-004 [open] [missing-input] (node M2)

- asked: 2026-11-14T22:30 by architect

Host `incusd` exposure for M2 live hypervisor integration (`ADR-0002`): This OpenCode session runs inside an unprivileged LXC/Incus guest (`/dev/mapper/chulak-incus` XFS with working `FICLONE` + `unshare` `netns`, guest `/dev/incus/sock` only, no `/var/lib/incus/unix.socket` or `/dev/kvm`). M1 proved `ReflinkNetnsEngine` live and tested `IncusEngine` against a Unix-socket REST harness. For M2 live `IncusEngine` (LXC + QEMU stateful RAM snapshot) tests, should we mount a dedicated project-restricted Incus Unix/HTTPS endpoint into this container, or run nested `incusd`?

**Answer:**

## H-005 [closed] [direction-challenge] (node M1.6)

- asked: 2026-11-14T23:57 by lead

World/project identity has no single owner in the DI tree, so the M1.6 .localhost edge + ADR-0005/0006 design cannot be drawn cleanly. Exact conflicts: (a) SnowFile.project is parsed+validated+documented as the slug source but never consumed; the real slug is derived from the checkout dir name (crates/slipfork/src/main.rs:157,256,395), so 'project: acme' in a dir named myrepo silently mints http://myrepo.localhost. Who owns project identity: the manifest or the directory? (b) WorldTarget is defined in slipfork-snow, but ARCHITECTURE.md:129 puts it in WorldSpec consumed by slipfork-engine (which does not depend on slipfork-snow), and ARCHITECTURE.md:67 calls the same concept WorldId in slipfork-edge: 3 names, 3 owners. (c) LocalHostRoute.fork: Option<String> erases UserFork vs TestSlot, so WorldRegistry must re-parse the reserved 'test-' prefix already enforced in validate_fork_name. (d) TunnelLease.stream: UnixStream puts an OS FD in a domain trait return type, blocking any second TunnelRegistrar. OPTION A (recommended): create a tiny dependency-free slipfork-world crate owning WorldTarget/WorldId + the opaque TunnelLease trait; slipfork-snow, slipfork-engine, slipfork-edge all depend on it; make snow.yml 'project:' mandatory and delete the dir-name slug fallback entirely; LocalHostRoute { target: WorldTarget, snow_domain: Option<String> }; TunnelLease { public_url, lease_id } with WeavedUnixRegistrar keeping the UnixStream private. OPTION B: keep identity in slipfork-engine, add slipfork-engine as a dependency of slipfork-snow (inverts current layering, makes the manifest crate depend on the engine), keep the dir-derived slug and DELETE SnowFile.project + its docs. Recommendation: Option A. Also needs a decision on domains[].id 'test-*' (currently accepted and shadowing TestSlot routing) - reserve it by validation.

**Answer:**
Approved Canonical repo dir: keep `snow.yml` 12-factor pure (delete `SnowFile.project` and any `project:` docs in `snow.yml`); derive `<project>` from the canonical VCS root directory name (so `.worktrees/<fork>` still resolves to the canonical repo folder slug), with an optional `project = "..."` override in `slipfork.toml`; create a pure zero-OS-leak `crates/slipfork-world` crate owning `WorldTarget { Canonical, UserFork(String), TestSlot(u32) }`, `LocalHostRoute { project: String, target: WorldTarget, snow_domain: Option<String> }`, and opaque `trait TunnelLease: Send + Sync` / `trait TunnelRegistrar`; reserve `test-*` in `snow.yml` `domains[].id` validation.

## H-006 [closed] [missing-input] (node M1.6)

- asked: 2026-11-14T23:57 by lead

Tunnel Weaver daemon ('weaved') IPC wire contract is asserted as an existing external contract in ADR-0005-localhost-edge-weaved-ipc-and-vcs-remote.md:24 and ARCHITECTURE.md:193 ({"op":"register_tunnel","version":1,...} request / {"ok":true,"lease_id":...} reply, socket path, lease-revocation-on-disconnect semantics), but no human-confirmed source exists (plan/HUMAN.md has none; H-004 is incusd-only). M2 will implement against this payload, so shipping it unconfirmed is an invented external API. Provide either (1) the real weaved protocol doc/schema + socket path + version-negotiation and revocation semantics, or (2) explicit approval that Slipfork OWNS and defines this contract (then it must be published as Slipfork's own spec with a versioned schema + conformance suite, not cited as weaved's existing behavior in ADR-0005/ARCHITECTURE.md), or (3) approval to mark it 'Slipfork-proposed, pending confirmation' everywhere until the daemon exists. Recommend (3) for the M1.6 doc sweep with a versioned schema file owned by Slipfork.

**Answer:**
Approved Defer wire spec: delete all invented `weaved` JSON wire payloads/socket paths from `ARCHITECTURE.md` and `ADR-0005`; keep only the pure Rust `TunnelRegistrar` + opaque `TunnelLease` DI traits (with an in-memory conformance test), and state explicitly that the concrete `WeavedRegistrar` wire adapter is gated on `tunnel-weaver` shipping `weaved`'s real IPC specification.

