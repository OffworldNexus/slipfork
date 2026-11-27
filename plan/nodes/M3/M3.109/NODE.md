---
id: M3.109
title: "Registry authentication and a 'slipfork docker' namespace (login/logout/build/pull/images)"
kind: epic
status: done
priority: 3
depends_on: [M3.107, M3.108]
paths:
  - "crates/slipfork-image/**"
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/compose.rs
  - "crates/slipfork/src/daemon/**"
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/surface.rs
  - "crates/slipfork/src/world/**"
  - "crates/slipfork-world/tests/critical_pod_runtime.rs"
  - "crates/slipfork/tests/**"
  - "packaging/man/**"
  - "specs/02-cli-ux.md"
  - "specs/03-under-the-hood.md"
  - docs/cli.md
  - TESTING_PLAN.md
  - Makefile
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-24T12:52"
updated: "2026-11-24T16:28"
---

# Goal

Private registries and rate limits need credentials; the owner also wants a discoverable Docker-like surface that does not pretend to be a Docker daemon.

# CLI surface (owner's words: `slipfork docker login`, `slipfork docker build <component>`)

- `slipfork docker login [HOST]` — default `docker.io`; `--username`; password from a TTY prompt (no echo) or `--password-stdin`; never argv. Verify the credential against the registry token endpoint before saving.
- `slipfork docker logout [HOST]`, `slipfork docker list` (hosts + username, never the secret).
- `slipfork docker build <component> [--target <stage>] [--json]` — resolves the component from the Forkfile, builds through the existing image engine with the same live per-step events as `up`, prints the image digest/tag.
- `slipfork docker pull <ref>` / `slipfork docker images` — pull into the store and list cached images.
- `--json` for every verb; one `next:` on failure.

# Auth design

- OCI Distribution v2: on 401 parse `WWW-Authenticate: Bearer realm=…,service=…,scope=…`, fetch a token with Basic auth, retry; support `Basic` directly where the registry does.
- Credential store: `<config>/slipfork/registries.json`, mode 0600, atomic write; host → `{username, secret}`. Read Docker's `~/.docker/config.json` `auths` as a fallback (never write it). Never log or embed credentials in images or build args.
- The daemon/composition root resolves the store once and injects it into the puller; library crates read no env/files. `COPY --from`/`FROM` external refs use the same auth.

# Acceptance

- Real lab against a minimal loopback OCI registry with a 401 bearer challenge: `login --password-stdin`, `pull`, `docker build` of a component whose `FROM` and `COPY --from` are private; wrong password → one line + next; stored file is 0600 and the secret never appears in logs or `--json`.
- `slipfork docker build` streams the M3.107 step/log events; `--json` pure.
- Docs/spec: `docs/cli.md`, `specs/02-cli-ux.md`, `specs/03` port map; no Docker-daemon claims.
- `make check`, `make docs-check`; commit `[M3.109] ...`, done, no push.
