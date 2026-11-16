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

## H-009 [closed] [direction-challenge] (node M2)

- asked: 2026-11-15T11:45 by strategist

ADR-0007 tier classification for shipped, conformance-tested adapters lacking a published recipe page (e.g. incus-engine in 0.1.0): ADR-0007 §1 defines tier 'none' as 'No meaningful test surface; nothing to integrate (e.g. a pure CLI library)' and 'declared' as 'Manifest recipe written' (requiring recipe_page + critical_test). In 0.1.0, incus-engine ships as a Unix-socket REST /1.0 library adapter passing the shared WorldEngine conformance suite while CLI/live wiring awaits H-004 (so publishing a recipe_page would violate Zero Vaporware), forcing 0.1.0 to record it conservatively as tier = 'none' with a disclosure in rationale. Before M2 exposes 'slipfork support [--json]' and generates the public docs matrix: Option A — add an intermediate machine-verified rung 'harnessed' (none < harnessed < declared < exercised < field-tested < hardened) requiring design_doc + critical_test for in-tree adapters whose user CLI/recipe surface has not shipped yet. Option B (recommended) — keep the 5-tier scale strictly scoped to user-reachable integrations: omit pre-CLI internal library adapters from support/integrations.toml until wired + documented at 'declared', keeping 'none' reserved strictly for non-integrations / pure CLI libraries.

**Answer:**
Approved Option B (ratified per owner Zero-Vaporware & tier-honesty directives): keep the 5-tier scale (`none`, `declared`, `exercised`, `field-tested`, `hardened`) strictly scoped to user-reachable integrations; omit pre-CLI internal library adapters from `support/integrations.toml` until wired to CLI + documented at `declared`, reserving `none` strictly for platforms/libraries with no meaningful lifecycle/test integration surface.

## H-010 [closed] [direction-challenge] (node M2.1)

- asked: 2026-11-15T17:32 by lead

M2.1 locked ADR-0008 + ADR-0001/0003/0005/0007 + ARCHITECTURE.md + target-DX (all gates green), but three linked cross-node DI/wire blind spots block M2.2/M2.6/M2.8/M2.9/M2.13/M2.16/M2.18 leaf work; architect halted rather than invent. FULL DETAIL: plan/nodes/M2/M2.1/RESULT.md. (1) Cross-process WorldEngine/WorldHandle: the CLI (M2.16/M2.17) materializes WorldSpec and owns ExecEnv secrets + PTY fds, while slipforkd (M2.8/M2.9) must dial_world_tcp/release worlds but only gets register_world(project, target) with no handle lookup, snow_domains, or default_ingress_port. A1 (recommended): keep secrets/PTY in the CLI; add state-verified WorldEngine::open_world(project, target) -> WorldHandle (validated against persisted init_pid/starttime or incusd) plus DaemonControl::register_world(WorldRegistration { project, target, snow_domains, default_ingress_port }); default ingress = ingresses[prefix '/'] -> ingresses[0] -> components[0].internal_ports[0]. B1: move WorldEngine + SecretProvider inside slipforkd and pass PTY fds via SCM_RIGHTS (bigger, secrets cross process). (2) Portless WorldOrigin::Local (ADR-0005 §1) is unreachable when slipforkd --listen 127.0.0.1:<port!=80> (mandatory on this host: :80 is EACCES, and systemd-socket-activate/M2.18 e2e need :0) and inside a world netns where host :80 does not exist. A2 (recommended): WorldOrigin::local(&route, ingress_port) renders http://<host> at :80 and http://<host>:<port> otherwise, with a lightweight in-netns loopback forwarder so $BASE_URL resolves identically on host and in slipfork exec/test; B2: keep portless :80 only (breaks zero-config $BASE_URL and M2.18 on unprivileged hosts). (3) tpmd framing: M2.13 says 'line-framed' but live tpmd 0.1.1 (tpmd-proto/src/frame.rs:14) uses u32be length + postcard binary framing and tpmd-proto is 404 on crates.io. A3 (recommended): add postcard 1 (alloc) to M2.2, lock the u32be+postcard tpmd v1 framing + golden byte vectors in ADR-0008, and have M2.13 verify golden vectors + live /run/tpmd/tpmd.sock; B3: gate live TpmdDeviceKeySigner in an isolated leaf and ship only SoftwareP256Signer in M2.13. Reply 'A1, A2, A3' (or deviations) and M2.1 resumes to fold the ruling into ADR-0008/ARCHITECTURE/target-DX and amend the downstream NODE.md files; nothing else in M2 can start before that.

**Answer:**
Owner answered all three parts of H-010:
1. **Unified User Daemon + Direct CLI TTY (`slipforkd` owns worlds, OSPP & secrets; zero `SCM_RIGHTS`):** Because `slipforkd` runs unprivileged as the developer's own user (`User=%i`, receiving `:80` via `LISTEN_FDS`), `slipforkd` is the single local authority that holds the persistent OSPP WebSocket session, pulls/resolves `snow.yml` secrets & resources, boots/supervises background CoW worlds (injecting resolved env vars into running components), and routes `.localhost` (`snow.yml` `ingresses` prefixes) + shared tunnel traffic. `slipfork` CLI talks to `slipforkd` over the `0600` user control socket (`$XDG_RUNTIME_DIR/slipfork/daemon.sock` / `<state_root>/daemon.sock`); for interactive TTY commands (`slipfork shell`, `db`, `redis`, `hook run`), `slipforkd` returns the world target/PID + resolved env map over the `0600` socket and the CLI enters the world directly inheriting its own native terminal stdin/stdout/stderr (zero `SCM_RIGHTS` FD passing!).
2. **Approved A2 (`WorldOrigin::local(&route, ingress_port)` + in-netns loopback bridge):** Renders portless `http://<host>` when `ingress_port == 80` and `http://<host>:<port>` under unprivileged `--listen`, with an in-netns loopback TCP bridge so `$BASE_URL` resolves identically from both the host browser and processes inside the world's `netns`.
3. **Approved Git Dependency on `OffworldNexus/tpmd` (matching `tunnel-weaver/crates/weave/Cargo.toml`):** Do NOT hand-roll `postcard` framing! Add `tpmd-client = { git = "https://github.com/OffworldNexus/tpmd", rev = "90cb2db45b3ea6bc065918f11c7e0fd23b4ce837" }` and `tpmd-ipc = { git = "https://github.com/OffworldNexus/tpmd", rev = "90cb2db45b3ea6bc065918f11c7e0fd23b4ce837" }` in `M2.2` / `slipfork-ospp` (`M2.13`), exactly as `tunnel-weaver` (`crates/weave/Cargo.toml:53-55`) links `tpmd` against `/run/tpmd/tpmd.sock`, with `SoftwareP256Signer` fallback when `tpmd` is absent.

## H-011 [closed] [direction-challenge] (node M2.7)

- asked: 2026-11-15T19:25 by lead

Netns entry from slipforkd is impossible as specified (proved on this host, details plan/nodes/M2/M2.7/RESULT.md): setns(2) needs CAP_SYS_ADMIN in the caller's userns AND the namespace's owner, and setns(CLONE_NEWUSER) is EINVAL in any multithreaded process — so no slipforkd thread can ever enter an unprivileged world netns (thread setns -> EPERM/EINVAL measured; a single-threaded nsenter child DOES enter and dials in-world 127.0.0.1). Consequence: dial_world_tcp, the in-netns :80 bridge (sysctl part is fine: per-netns ip_unprivileged_port_start write works) and spawn_service readiness cannot be the in-process worker ARCHITECTURE.md §3.3 describes. Options: (A, engine's recommendation) slipforkd re-execs itself single-threaded under 'unshare --user --map-user=$UID --map-group=$GID --keep-caps' at startup, so it owns the worlds' userns parent and the documented dialer/bridge worker THREADS work verbatim — no new binaries, no SCM_RIGHTS, but it changes the daemon posture, needs a userns re-exec fixture and M2.16/M2.17 wiring. (B) add a per-world in-netns slipfork-world-agent over an AF_UNIX socket with SCM_RIGHTS fd handoff — new binary + sendmsg/recvmsg in linux_cow.rs, and 'zero SCM_RIGHTS' must be reworded to 'never across the CLI/daemon boundary'. (C, lead) keep the daemon unprivileged and out of any userns: per-connection single-threaded helper child (the slipfork binary re-exec'd in a hidden nsenter mode, or nsenter + a byte relay) that enters the world netns and proxies bytes over its stdio pipes to slipforkd — no SCM_RIGHTS, no posture change, no new unsafe, at the cost of one short-lived child per proxied connection and a hidden re-exec entry point. SECOND, COUPLED DECISION: entry_command uses 'nsenter --target <init_pid>', but M2.6 worlds have no persistent init process (every exec is a throwaway unshare scope). Should worlds own a supervised init process from clone_world to destroy_world (and what happens to a live world during freeze_world)? Recommend: yes — boot a minimal world init at clone_world (also gives every world a stable netns), freeze only the on-disk state with components stopped first. Reply with A/B/C and the init-lifecycle ruling; M2.7 then implements in one sweep.

**Answer:**

## H-012 [open] [release-gate] (node M2)

- asked: 2026-11-16T06:41 by conductor

Releases 0.1.0 AND 0.2.0 are ready (they are one linear history on release/0.2.0 @ 12f0e83, both gated by strategist; 0.1.0 never had a remote so it was never pushed). ACTION NEEDED: still no git remote and 'gh repo *' is denied to agents. Please run: gh repo create OffworldNexus/slipfork --private --source=. --remote=origin && git push -u origin release/0.2.0 && git push -u origin develop   (or add the remote yourself). Then tell me and I will open the PR into develop. Everything else is green: RUST_TEST_THREADS=1 make check EXIT=0, bounded bench EXIT=0 in 36.45s, M2 milestone gate PASSED.

**Answer:**

