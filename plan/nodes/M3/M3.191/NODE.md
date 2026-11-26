---
id: M3.191
title: "Doctor Storage row copy, version stamps 0.1.1, ARCHITECTURE/specs drift"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/doctor/**"
  - "packaging/**"
  - "editors/**"
  - "packages/opencode-slipfork/package.json"
  - README.md
  - ARCHITECTURE.md
  - "specs/**"
  - CHANGELOG.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T23:40"
updated: "2026-11-26T03:01"
---

# Gate findings

1. **`doctor` Storage copy bug:** `doctor/probe.rs:94-111` builds `Part::Meta("on"/"not on") + Part::Bold("Btrfs")`, and `Check::new` joins parts with `" · "`, so the row reads `<path> · not on · Btrfs · …` in both terminal and `--json` `detail`. Collapse to a single part.
2. **Version stamps still 0.1.0:** `packaging/arch/.SRCINFO` (also missing `depends = slirp4netns` and `install = slipfork.install` from `PKGBUILD`), `packaging/man/slipfork-lsp.1:1`, `packages/opencode-slipfork/package.json:3`, `editors/zed/{extension.toml:4,Cargo.toml:3}`, `editors/tree-sitter-forkfile/{package.json:3,tree-sitter.json:14}`, `README.md:13`.
3. **`ARCHITECTURE.md`** still describes deleted `DECSTBM`/`ScrollRegionGuard`, `ImageCache`, `WorldStampStore`, `TableLayout` and the old fork timing.
4. **Spec drift:** `specs/02:290-308,331` missing `Done::Partial`/`Outcome::Problem` and the 4-sink print allowlist; `specs/03:373,378,380,545` names `PostgresTemplate`/`images/oci/<digest>`/`.slipfork/world.json`/`fork_checkouts` that no longer exist (or make §6.6/§7.5 truthful about what remains); `specs/06:19-26,67,69,81,90` needs `slirp4netns` in runtime contracts and `scripts/rpm/slipfork.spec` → `packaging/rpm/slipfork.spec`; `specs/README.md:6-12` sample output is pre-`Report`.

# Constraints

- **Do not edit `docs/`** — the owner is rewriting it. (For their attention only: `zensical build --strict` currently flags 3 broken `#on-ext4-or-xfs-…` anchors in `docs/getting-started.md:26` and `docs/troubleshooting.md:42,49`.)
- Behaviour claims must match the shipped binary; no ADR prose; `guard.py` + `check_docs_honesty.py` green.

# Acceptance

- `doctor` prints the Storage row without the double separator (terminal + `--json` pinned by a test); every version stamp reads 0.1.1; `ARCHITECTURE.md`/specs describe only shipped behaviour.
- `make check`; Triad-light (`review-ux`); commit `[M3.191] ...`, done, no push.
