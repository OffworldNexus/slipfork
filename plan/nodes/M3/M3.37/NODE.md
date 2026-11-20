---
id: M3.37
title: "Scorched-earth clean-sheet wipe: keep only specs/ & agentic setup (AGENTS.md, TESTING_PLAN.md, .opencode/, plan/, tools/)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths:
  - "specs/**"
  - TESTING_PLAN.md
  - AGENTS.md
  - ".opencode/**"
  - "plan/**"
  - "tools/**"
  - .gitignore
  - .editorconfig
  - rustfmt.toml
  - "crates/**"
  - "crates/*/tests/critical_*.rs"
  - "**/tests/critical/**"
  - "**/*.critical.test.ts"
  - Cargo.toml
  - Cargo.lock
  - ARCHITECTURE.md
  - CHANGELOG.md
  - README.md
  - Makefile
  - zensical.toml
  - "docs/**"
  - "editors/**"
  - "packages/**"
  - "poc/**"
  - "support/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T00:44"
updated: "2026-11-21T00:44"
---

# Goal

Delete everything in the repository except the clean-sheet `specs/` suite and the agentic harness (`AGENTS.md`, `TESTING_PLAN.md`, `.opencode/`, `plan/`, `tools/`, `.gitignore`, `.editorconfig`, `rustfmt.toml`), while first capturing the complete external `OSPP v1` & `snow.yml` wire/migration contracts in `specs/05-ospp-v1.md` and the multi-distro Linux packaging & documentation spec in `specs/06-distribution-and-docs.md` so downstream clean-sheet implementation has 100% self-contained specs.

# Exact Steps

1. **Before deleting old code/docs, make `specs/` 100% self-contained:**
   - Read `docs/design/ospp.md`, `crates/slipfork-ospp/src/{wire.rs,error.rs,device_key.rs,dump.rs}`, and `crates/slipfork-forkfile/src/migrate/` and author concise, authoritative **`specs/05-ospp-v1.md`** containing:
     - WebSocket `Sec-WebSocket-Protocol: ospp-v1`, fail-closed `version: 2`, all 16 JSON frame schemas, all 12 error codes (`unsupported_version`, `unsupported_auth`, `unsupported_encryption`, `unauthorized_key`, `unknown_remote`, `bad_proof`, `rejected`, `already_paired`, `throttled`, `unresolved`, `dump_unavailable`, `bad_frame`), ALTCHA+PoP `#handle=` browser pairing saved to `<state_dir>/ospp.json` (`{endpoint, account, machine}`), 33-byte compressed P-256 device pubkey + 64-byte raw `r||s` ECDSA signature, Git-remote `subscribe` carrying evaluated `Forkfile`, `hpke-p256` secret encryption (`info="ospp-v1-secret"`, `aad="<service>:<env_key>"`), and HTTPS `dump_ready` stream framing (`65`-byte `enc`, `4`-byte BE chunk len + `ctx.Seal`, `len=0` EOF, `info="ospp-v1-dump"`, `MAX_DUMP_HEADER_BYTES=16 KiB`, `MAX_DUMP_CHUNK=1 MiB`, mandatory `Content-Length`, zero redirects).
     - Legacy `snow.yml` input schema & `${{ ... }}` template rules for `slipfork migrate` -> `# forkfile: 0.1` (mapping services into enclosing `image` scopes, `resources.{pg_clusters,pg_databases,redis_clusters,s3_buckets}`, `domains`, `env_groups`, `routes`).
   - Author concise **`specs/06-distribution-and-docs.md`** modeled on `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0` (`scripts/build-deb.sh`, `scripts/build-rpm.sh`, `scripts/build-standalone.sh`, `docs/install.sh`, `.github/workflows/{ci,docs,packages,nightly}.yml`) and `OffworldNexus/tpmd` (`debian/` socket-activated systemd units, postinst/prerm, Cloudsmith + Forgejo publishing):
     - Native packages for **all Linux distros** across `x86_64`/`amd64` and `aarch64`/`arm64`: Debian/Ubuntu `.deb` (`dpkg-deb --root-owner-group`, measured `GLIBC_X.Y` floor, `crun` + `btrfs-progs`, socket-activated `:80` ingress units, `/etc/apparmor.d/slipfork`, `slipfork(1)` manpage, `bash`/`zsh`/`fish` completions, `lintian`-clean), Fedora/RHEL/Rocky/Alma/openSUSE `.rpm` (`rpmbuild`), Arch Linux `.pkg.tar.zst` + AUR `PKGBUILD`/`.SRCINFO`, Alpine Linux `.apk` / static `musl` + OpenRC `:80` service, portable `gnu` + `musl` `.tar.gz` archives + `SHA256SUMS` + `scripts/check-portable-elf.sh`, and universal `docs/install.sh` (Cloudsmith `offworld-nexus` + Forgejo `forge.offworld.nexus` native repos + SHA-256 standalone fallback + `--user`/`--prefix`/`--develop`).
     - Visual Zensical documentation site (`zensical.toml`, `README.md`, `CHANGELOG.md`, `docs/*.md`, `Forkfile` syntax highlighting, `uvx --offline zensical build --strict`, `tools/check_docs_honesty.py`).
   - Update `specs/README.md` to index `01`..`06`, update `specs/04-testing-plan.md` §3 to include multi-distro Linux package installation & `install.sh`, and write root **`TESTING_PLAN.md`** matching `specs/04-testing-plan.md`.
2. **Preserve verified Linux/Btrfs/`crun`/namespace host lessons in `.opencode/LESSONS.md`:**
   - Move the verified kernel/Btrfs/`crun`/namespace/VM lessons from `docs/knowledge/LESSONS.md` to `.opencode/LESSONS.md` (stripping stale `M1`/`M2`/`M3` archaeology and fixing any deleted script mentions).
3. **Scorched-earth delete everything outside `specs/` and the agentic setup:**
   - Remove (`git rm -rf` + clean untracked/modified working-tree remnants, including `site/`): `crates/`, `Cargo.toml`, `Cargo.lock`, `ARCHITECTURE.md`, `CHANGELOG.md`, `README.md`, `Makefile`, `zensical.toml`, `docs/`, `editors/`, `packages/`, `poc/`, `support/`.
4. **Update the agentic setup cleanly:**
   - Rewrite `AGENTS.md` concisely for the clean-sheet `specs/` & `TESTING_PLAN.md` rules (pointing to `specs/`, `TESTING_PLAN.md`, `.opencode/LESSONS.md`, Constructor IoC/DI, lab-first E2E testing, and multi-distro packaging).
   - Update `.opencode/opencode.jsonc` (add read-only `external_directory` allow + `edit` deny for `~/offworld-nexus/tpmd/*`) and scrub stale `slipfork-snow` / `snow.yml` mentions in `.opencode/{agents,skills}/**`.
   - Update `plan/ROADMAP.md` for clean-sheet `0.1.0` (`M3.37..M3.42`) and raise `plan/BUDGET.json` `expensive_cap_usd` to `750`.
   - Clean `tools/`: update `tools/guard.py` (`Forkfile` instead of `snow.yml`), keep `tools/plan.py` and `tools/critical_guard.py`, update `tools/check_docs_honesty.py` and `tools/do_vm.py` cleanly for single-binary `slipfork` + `slipfork-world-init` (zero `slipforkd` / `git-remote-slipfork` / `snow.yml`), add `tools/coverage_report.py` (modeled on `blastit/.worktrees/release-0.2.0/tools/coverage_report.py` writing into `target/cov`), remove obsolete `tools/rs_test_list.py` and `tools/check_zed_extension.py`, and ensure `tools/tests/` passes 100%.

# Acceptance

- `git ls-files` and `git status --short` leave ONLY `specs/**`, `TESTING_PLAN.md`, `AGENTS.md`, `.opencode/**`, `plan/**`, `tools/**`, `.gitignore`, `.editorconfig`, `rustfmt.toml` (zero `crates/`, `docs/`, `Cargo.toml`, `editors/`, `packages/`, `poc/`, `support/`).
- `uv run --no-project tools/plan.py validate` exits `0`.
- `uv run --no-project tools/guard.py` exits `0`.
- `uv run --no-project -m unittest discover -s tools/tests` exits `0`.
