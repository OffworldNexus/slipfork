---
id: M3.82
title: "Support ADD http(s):// sources in the native Dockerfile builder"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-image/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T00:14"
updated: "2026-11-24T00:23"
---

# Goal

`ADD https://offworld-nexus.app/install.sh /tmp/install.sh` currently fails with "fetches a URL, which the native builder does not support". Owner request: support `ADD <url>`.

# Contract

- `ADD` accepts absolute `http://` and `https://` sources; `COPY` keeps rejecting them. Any other scheme (`file:`, `ftp:`) fails closed with one line + next.
- Fetch with the existing `ureq` dependency (already used by the puller): follow redirects, fail closed on non-2xx, stream to a temp file in the build dir, enforce a documented size cap (e.g. 2 GiB) and a timeout.
- Destination semantics match Docker: a trailing `/` or existing directory appends the last URL path segment; otherwise it is the file path. Mode `0644`, `chown` to the step `Owner` like every other copied file, resolved through the existing `Destination`/`WORKDIR` helper.
- URL payloads are **not** tar-extracted (local archives keep extracting).
- Keep the current layer/instruction cache behaviour; document that a URL layer is cached by instruction text (no checksum flag yet).
- Errors: `1 line + next:` naming the URL, the status/transport failure, and a remedy.

# Pre-baked findings

- `crates/slipfork-image/src/copy.rs:129-165` `add_context`; the URL rejection is at ~141. `copy_context`, `Destination`, `Owner`, `chown` are in the same module.
- Parser/instruction: `dockerfile/parse.rs:158`, `instruction.rs:138`, `config.rs:112`.
- `ureq` is a dependency of `crates/slipfork-image/Cargo.toml` (used by `src/puller.rs`); reuse its agent/TLS configuration, do not add a second HTTP stack.
- Tests: `crates/slipfork-image/tests/critical_dockerfile_live.rs` (real builds) and `tests/image_core_lab.rs`; the repo already uses minimal loopback servers for real-network tests — serve bytes from `127.0.0.1:0` rather than hitting the internet.

# Acceptance

- Real loopback test: `ADD http://127.0.0.1:<port>/payload.txt /opt/data/` writes `/opt/data/payload.txt` with the served bytes; a non-2xx returns a typed error; `COPY http://…` still fails.
- `cargo test -p slipfork-image`; `make check`; a live build of a Dockerfile using a local loopback URL.
- Commit `[M3.82] ...`, done, no push. Leave `docs/**` alone (the packaging node owns it); doc-comment the new behaviour.
