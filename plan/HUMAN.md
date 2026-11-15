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

## H-007 [open] [release-gate] (node M1)

- asked: 2026-11-15T02:32 by conductor

Release 0.1.0 is ready on branch release/0.1.0 (head 5630a1e, make check green, M1 final gate passed by strategist at 6ee3b7b, CHANGELOG 0.1.0 dated 2026-11-15). NOTE: the design of record was amended after the first gate pass to fold your four release-gate corrections (single canonical origin, support tiers, typed coverage with no shell recipes, mode-scoped components kept) plus the H-008 ingress ruling; M1.8/M1.9 carry those and both passed their gates. ACTION NEEDED: this repo has no git remote yet and agent shell policy denies 'gh repo *', so I cannot push or open the PR. Please either (a) create the GitHub repo OffworldNexus/slipfork and run 'git remote add origin git@github.com:OffworldNexus/slipfork.git && git push -u origin release/0.1.0 && git push -u origin develop' (I will open the PR into develop afterwards), or (b) tell me to run the remote setup if you grant the permission. Nothing else blocks the release.

**Answer:**

## H-008 [closed] [direction-challenge] (node M1.8)

- asked: 2026-11-15T11:00 by lead

Single-origin invariant: who owns ingress for a Shared world (weaved forward target)? WorldOrigin::Shared{url} can only come from TunnelLease::public_url(), but TunnelLease is minted from TunnelRequest{route, local_port} against a live world port, while the origin is needed at boot (and before per_world fixture seeds). If weaved forwards to http://<fork>.<project>.localhost:80 the new 308 rule redirects it straight back to the shared URL (loop); if weaved forwards to the world port directly it needs setns/dial into the world, which is slipforkd's WorldEngine::dial_world_tcp capability, and LocalHostRoute::parse 404s non-.localhost Hosts. Nothing in the DI tree maps a shared hostname -> (ProjectSlug, WorldTarget). Recommended Option A: slipforkd is the sole shared ingress (shared-host map in WorldRegistry populated by share_world, proxying via dial_world_tcp with Host/X-Forwarded-* preserved); the 308/421 rule applies only to *.localhost; WorldOrigin::Shared is minted from the lease against a static planned port so origin precedes materialization. Option B (two-phase Local->Shared re-materialization) rejected: reopens a two-origin window that per_world seeds would bake in. Please confirm Option A / weaved forward target (H-006 related).

**Answer:**
RATIFIED BY CONDUCTOR on the owner's explicit directive at the 0.1.0 release gate ("a given environment is going to be either shared either not shared but will have at most one URL (and if you hit the local URL of a shared one you're just sent to the shared URL)").

**Option A confirmed.** It is the only shape that satisfies that sentence, because:
- the owner's sentence *prescribes* the 308 for the local URL of a shared world, so the redirect is correct behaviour, not a loop;
- therefore the tunnel must never forward to `*.localhost:80` (that would loop), so `weaved` forwards to `slipforkd`, which is the **sole shared ingress** and proxies into the world via `WorldEngine::dial_world_tcp` preserving `Host` + `X-Forwarded-*`;
- the 308/421 rule is scoped to `*.localhost` Hosts only; shared hostnames are proxied, never redirected;
- `WorldOrigin::Shared` is minted from `TunnelLease::public_url()` against a **planned static port** decided before materialization, so an origin exists before any `per_world = true` fixture seed runs;
- `WorldRegistry` owns the shared-hostname map (`shared hostname -> (ProjectSlug, WorldTarget)`), populated by `share_world` and cleared by `unshare_world`/lease drop.

Option B (two-phase Local -> Shared re-materialization) stays rejected: it reopens the two-origin window the owner closed, and `per_world` seeds would bake a stale origin into persistent state.

Cross-reference `H-006`: the concrete `weaved` forward target (shared hostname -> local ingress) is Slipfork's `slipforkd` endpoint; the wire contract for registering that lease remains deferred until `tunnel-weaver` publishes `weaved`'s IPC spec.

## H-009 [open] [direction-challenge] (node M2)

- asked: 2026-11-15T11:45 by strategist

ADR-0007 tier classification for shipped, conformance-tested adapters lacking a published recipe page (e.g. incus-engine in 0.1.0): ADR-0007 §1 defines tier 'none' as 'No meaningful test surface; nothing to integrate (e.g. a pure CLI library)' and 'declared' as 'Manifest recipe written' (requiring recipe_page + critical_test). In 0.1.0, incus-engine ships as a Unix-socket REST /1.0 library adapter passing the shared WorldEngine conformance suite while CLI/live wiring awaits H-004 (so publishing a recipe_page would violate Zero Vaporware), forcing 0.1.0 to record it conservatively as tier = 'none' with a disclosure in rationale. Before M2 exposes 'slipfork support [--json]' and generates the public docs matrix: Option A — add an intermediate machine-verified rung 'harnessed' (none < harnessed < declared < exercised < field-tested < hardened) requiring design_doc + critical_test for in-tree adapters whose user CLI/recipe surface has not shipped yet. Option B (recommended) — keep the 5-tier scale strictly scoped to user-reachable integrations: omit pre-CLI internal library adapters from support/integrations.toml until wired + documented at 'declared', keeping 'none' reserved strictly for non-integrations / pure CLI libraries.

**Answer:**

