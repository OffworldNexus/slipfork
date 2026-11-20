# Slipfork Specifications

**Your cloud environment locally in a box; CI is the clutch.**

From a single `Forkfile`, Slipfork builds and runs your container images, databases, caches, object buckets, and background workers behind one `.localhost` domain on your machine — pulling live cloud secrets when paired with CI, or generating local secrets offline. Because your project folder, image layers, and every backing service live natively on Btrfs + `crun` (**100% Docker-free**), `slipfork fork` clones your code, dependencies, input-hash stamps, and running databases together in milliseconds so humans and AI coding agents can work on branches in parallel.

```text
$ slipfork up
✓ Ready -> http://demo.localhost

$ slipfork fork feat-auth
✓ Forked demo -> feat-auth in 38ms -> http://feat-auth--demo.localhost
```

---

## Three Core Bets

1. **One zero-boilerplate `Forkfile` for cloud and local**
   Services live directly inside their `image` block (with build-time `arg`s cleanly separated from runtime `env`), `@uv` / `@pnpm` auto-wire deduplicated lockfile-watched install nodes, `before-start when changed(...)` handles custom codegen, and `features` / `on` + `secret()` keep cloud CI and local dev in lockstep.

2. **Pure Btrfs + `crun` end-to-end (zero Docker dependency)**
   Slipfork pulls OCI images over HTTPS and builds `Dockerfile`s natively as cached Btrfs step subvolumes, while your project directory itself is a Btrfs subvolume alongside your databases — so `slipfork fork` snapshots code, `.venv`, `node_modules`, hash stamps, PostgreSQL, Redis, and S3 buckets atomically in `< 50ms`.

3. **Instant, colorful, zero-noise CLI**
   One `.localhost` URL per world, one visual status tree with nested forks, real colors and spinners on interactive terminals, live logs across restarts, and a single per-user daemon with zero root/user split-brain.

---

## Specification Suite

- **[01 — The `Forkfile` Specification](./01-forkfile.md)** — `# forkfile: 0.1` header, enclosing `image` scopes & build `arg`s, runtime `env`/`secret()`, `s3` (`private` vs `cdn <id>`), `service`s, `before-start when changed(...)`, and `@<tool>` auto-prequels.
- **[02 — The CLI UX](./02-cli-ux.md)** — Visual status trees, live boot DAG progress, `< 50ms` forking, daily developer commands (`exec`, `shell`, `logs`, `db`), `doctor`, and strict terminal hygiene.
- **[03 — How It Works Under the Hood](./03-under-the-hood.md)** — Pure Btrfs subvolume layout, native Docker-free image engine, parallel World Boot & `@<tool>` DAG, per-service `crun` pods, single per-user daemon + `:80` ingress, `OSPP v1`, and OpenCode integration.
- **[04 — The Testing Plan](./04-testing-plan.md)** — End-to-end user stories and fail-closed safety boundaries verified against real containers, databases, and Btrfs snapshots.
