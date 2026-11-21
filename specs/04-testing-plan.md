# 04 — The Testing Plan

## 1. Testing Philosophy

Slipfork is verified **end-to-end through real developer workflows**: real Btrfs filesystems and subvolumes, real `crun` OCI containers, real native OCI pulls and `Dockerfile` builds, real PostgreSQL, Redis, and S3 services, real full-stack web applications, and the real `slipfork` binary. Every test verifies both the **promised developer experience** and the **containment or fail-closed safety boundary** when something goes wrong.

Focused non-E2E tests exist **only** for self-contained language, protocol, and editor rules whose edge cases are clearer to verify directly: `# forkfile: 0.1` header validation, `Forkfile` grammar and whole-file type checking, `@<tool>` / `before-start` DAG construction, secret-taint propagation, Merkle input-hash stamping, `OSPP v1` message framing, the language server, and the Zed extension. **No test exists unless it protects one of the user stories below.**

**Harness exceptions:** zero mocks — no `Mock*`, `Fake*`, `Stub*`, `Recording*`, in-memory vault, or `TestTreeDriver` shims; the only stand-in is the minimal `127.0.0.1:0` loopback OSPP reference server, and everything else uses real disk, subprocesses, and `crun` pods. Slow daemon/world boots consolidate into one journey per workflow, reaping `-pgid` process groups and Btrfs snapshots, and 0%-covered fallback code is deleted rather than tested.

---

## 2. Enclosing `image` Scopes, Ingress Parity & Backing Resources

- **Enclosing `image` builds, build-time `arg` vs runtime `env`, and OCI pulls:**
  Build a multi-stage `Dockerfile`-backed `image <id> <dir> { ... }` (`FROM ... AS`, `ARG`, `COPY --from`, `RUN --mount=type=cache`, `.dockerignore`) natively on Btrfs + `crun` with **no external container daemon installed** — passing declared non-secret `arg` values (`arg VITE_CDN_URL = static_cdn.url`), keeping runtime `env` strictly out of `Dockerfile` builds, inheriting rootfs, `<dir>` `workdir`, and `use env` across nested `service`s and hooks, and pulling OCI `image ... from <ref> { ... }` (`mailpit`) over HTTPS using its baked-in entrypoint without requiring `run:`.
- **Cloud-identical HTTP/WebSocket ingress (`route`, `domain`, `port` / `PORT`):**
  Verify ingress parity locally — preserving matched path prefixes by default (`route /api` forwards `/api/users` as `/api/users`, emitting `preserve_path_prefix: true` in cloud), stripping prefixes on `route /api strip-prefix`, rewriting prefixes on `route /api rewrite /v1`, matching exact paths on `route exact /healthz`, filtering hosts across primary `<world>.localhost`, extra `domain <id>` (`<id>.<world>.localhost`), and subdomain prefixes (`route / domain mail.*` -> `mail.<world>.localhost`), and auto-injecting `PORT` alongside `$port` and `internal_ports`.
- **`@<tool>` auto-prequels, `before-start when changed(...)`, and `s3` `private` vs `cdn <id>`:**
  Synthesize a single deduplicated `(image, lockfile_dir)` setup node across `@uv` / `@pnpm` services and hooks alongside custom `before-start when changed(...)` steps; wire services to PostgreSQL 18, Redis 7, a default-private `s3 uploads` bucket (`403` on anonymous `GET`), and an `s3 assets { cdn static_cdn }` bucket (`public-read` anonymous `GET` via `static_cdn.domain` and `static_cdn.url` at `http://static_cdn.<world>.localhost`).
- **Fail-closed static checks before boot:**
  Refuse `Forkfile`s missing `# forkfile: 0.1`, colliding names in the flat global symbol namespace, combining mutually exclusive `strip-prefix` and `rewrite`, referencing undeclared `domain <id>`s, or passing secret-tainted values into `image` `arg` across any `on` branch before starting a single container.

---

## 3. Installation, Btrfs Readiness & Migration

- **Single-binary install and `:80` `.localhost` ingress:**
  Install `slipfork`, run `slipfork daemon install` for the per-user unit, and `sudo slipfork daemon install --ingress` for the system `:80` socket and AppArmor profile on supported Linux distributions, routing port-less `http://<world>.localhost`, domain/CDN subdomains, and WebSocket traffic through the socket-activated `:80` proxy while keeping every world under the single per-user daemon with zero root/user split-brain.
- **Transparent Btrfs setup and daemon-free `slipfork doctor`:**
  Convert a plain git checkout on a Btrfs drive into a Btrfs subvolume automatically on first `slipfork up`, and surface clear `✓ Ready` / `✗` remedies in `slipfork doctor` across the only three host requirements: Btrfs filesystem support, unprivileged namespaces / AppArmor, and `crun` + `:80` ingress.
- **Native packages across every Linux distribution (`x86_64` / `aarch64`):**
  Install `.deb`, `.rpm`, `.pkg.tar.zst` (plus AUR `PKGBUILD`), and `.apk` from the signed repositories, and the portable `gnu` / `musl` `.tar.gz` archives, on Debian/Ubuntu, Fedora/RHEL/Rocky/Alma/openSUSE, Arch, and Alpine — each verified by its own package manager, by `scripts/check-portable-elf.sh` (measured `GLIBC_X.Y` floor, no `NEEDED libpython*`, no build-host `RUNPATH`), and by booting one world after install.
- **Universal `docs/install.sh` and `.localhost` `:80` ingress:**
  Take the native signed-repository path when a package manager and usable `sudo` exist (Cloudsmith primary, Forgejo mirror), and the SHA-256-verified standalone archive path for `--user`, `--prefix`, `--develop`, musl hosts, or a missing package manager — then confirm the OpenRC/systemd ingress still answers port-less `http://<world>.localhost` and WebSocket traffic.
- **Legacy `snow.yml` migration (`slipfork migrate`):**
  Convert real-world `snow.yml` manifests into clean, canonically formatted `# forkfile: 0.1` `Forkfile`s with nested `image` scopes and `route` / `domain` rules that pass whole-file checking and boot without manual edits.

---

## 4. Local Development & CLI UX

- **Visual status tree and scoped `ps`:**
  Render active worlds, services, backing resources (`s3 (private)` vs `s3 + cdn`), route rules (`:8000 -> /api`, `:8025 -> mail.*/`), ages, and nested forks in bare `slipfork` and `slipfork ps`, emitting semantic ANSI colors on interactive TTYs, zero escape sequences under `NO_COLOR`, `TERM=dumb`, or pipes, and identical data under `--json`.
- **Live logs, `exec` / `shell` ad-hoc commands, and database consoles:**
  Stream merged multi-service logs in `slipfork logs -f` seamlessly across `slipfork restart` with zero raw spinner escape garbage; run ad-hoc test suites and CLI commands directly via `slipfork exec [service|image] -- <cmd>` and `slipfork shell [service|image]`; and attach to databases via `slipfork db` and `slipfork redis`.
- **Headless-safe browser launch (`slipfork open`):**
  Open world and service URLs cleanly on desktop sessions while handling SSH/headless terminals gracefully without `sudo` environment-stripping regressions.

---

## 5. Instant Btrfs Project + World Forking & Coding Agents

- **Atomic project, stamp & database/S3 CoW forks (`slipfork fork` & `drop`):**
  Fork a running world with populated `.venv` / `node_modules`, `.slipfork/` input-hash stamps, and live PostgreSQL/Redis/S3 state in milliseconds — skipping unchanged `@<tool>` and `when changed(...)` steps on fork boot, re-running them only when the fork edits a lockfile or watched input, keeping code, bucket objects, and database rows completely separated from the parent world, and refusing `slipfork drop` on dirty checkouts without `--force`.
- **Parallel AI coding-agent sessions (`@slipfork/opencode`):**
  Open and close concurrent OpenCode agent worktrees against one repository, giving each agent session its own Btrfs fork and `.localhost` domain and reaping all containers, process groups, and snapshots cleanly when the session ends.

---

## 6. Remote Cloud Clutch & Zero-Trust Secrets (`OSPP v1`)

- **End-to-end encrypted secrets and live rotation:**
  Pair via `slipfork auth`, boot a world with `hpke-p256`-encrypted secrets from an `OSPP v1` provider, push an upstream secret rotation to restart affected services automatically, and seed a database from an encrypted remote dump — verifying secrets stay strictly on in-memory `tmpfs`, never touch disk, image layers, or `/proc/<pid>/cmdline`, and remain redacted in logs and CLI output.
- **Offline fallback parity:**
  Boot the same `Forkfile` offline with `secret(KEY) ?? random_hex(32)` generators, persisting local fallback secrets safely and gating `--reveal` strictly to interactive local TTYs.

---

## 7. Language, Wire Protocol & Editor Tooling (Focused Coverage)

- **`Forkfile` header, ingress / `arg` / `s3 { cdn <id> }` rules, `@<tool>` expansion & formatting:**
  Verify `# forkfile: 0.1` version checking, `route` grammar (`exact`, `strip-prefix` vs `rewrite` mutual exclusion, `domain <id | prefix.*>` authority checks), `s3 { cdn <id> }` and `domain <id>` `.domain` / `.url` typing, flat global symbol uniqueness, secret-taint rejection in `arg`, workspace lockfile discovery and `(image, lockfile_dir)` DAG deduplication, boolean `on` expressions, multi-branch `line:col` checking, and comment-preserving `slipfork fmt` / `slipfork fmt --check`.
- **`OSPP v1` wire framing, `slipfork-lsp` & Zed extension:**
  Verify `OSPP v1` P-256 challenge-response authentication, fail-closed `version: 2` validation, and live `Forkfile` diagnostics, `@<tool>` hover tooltips, go-to-definition, and format-on-save in the standalone `slipfork-lsp` binary and the Zed extension that resolves it.
