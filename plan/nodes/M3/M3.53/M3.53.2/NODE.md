---
id: M3.53.2
title: "Rewrite install docs & installer to canonical *.offworld-nexus.app repositories"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: [M3.53.1]
paths: ["docs/** README.md specs/06-distribution-and-docs.md AGENTS.md CHANGELOG.md"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-22T19:20"
updated: "2026-11-22T19:36"
---

# Goal
Replace every Cloudsmith/Forgejo mirror instruction with the canonical `*.offworld-nexus.app` repository snippets so a user copy-pastes apt/rpm/apk/arch/dl commands that actually work. **LOAD SKILL `offworld-packaging` FIRST** and use its §5 snippets verbatim, adapted only to the formats Slipfork really ships. Public pages must never name an internal host.

# Context
Anchors verified 2026-11-22:
- `docs/install.md:28-40`: flag table documents `--mirror cloudsmith|forgejo` (`:33`). Rewrite the page around the five canonical snippets.
- `docs/install.sh:16-17` (`CLOUDSMITH_BASE`, `FORGEJO_BASE` defaults), `:22` `MIRROR=cloudsmith`, `:50` usage text, `:70` parse, `:83` validation `case "$MIRROR"`, `:249-315` Forgejo/Cloudsmith install branches (`setup.deb.sh`, `setup.rpm.sh`, generic artifact fallback), `:315` develop/user/prefix condition keyed off `MIRROR=forgejo`. Delete the mirror concept end-to-end; keep `--version`, `--nightly`, `--user`, `--prefix <dir>`, `--develop [ref]`, `-y`.
- Preserve the `SLIPFORK_ARCHIVE_URL` + `SLIPFORK_SHA256` override path: `scripts/check-install.sh:41-42` drives it (M3.53.1 removed that script's `--mirror` flag). Digest verification for `--develop` stays mandatory.
- `README.md`, `specs/06-distribution-and-docs.md`, `AGENTS.md` reference registries/mirrors — align them; `CHANGELOG.md` gets one migration line.

Canonical forms: deb `Types: deb`, `URIs: https://apt.offworld-nexus.app`, `Suites: any-distro`, `Components: main` (and `nightly`), `Signed-By: /etc/apt/keyrings/offworld-nexus.gpg`; rpm `https://rpm.offworld-nexus.app/{stable,nightly}/$basearch/` with `gpgcheck=1`, `repo_gpgcheck=1`, `gpgkey=https://rpm.offworld-nexus.app/gpg.key`; apk `https://apk.offworld-nexus.app/{stable,nightly}` with `packages@offworld.nexus.rsa.pub`; arch `Server = https://arch.offworld-nexus.app/stable/$arch`, key `E9DD32C63321505EBBCF86386E08120B3306C720`; tarballs `https://dl.offworld-nexus.app/tarballs/{stable,nightly}/...`. Do NOT document `deb-src` (no `.dsc` ships).

Doc craft: user value + a working snippet inside the first 25 lines, Mermaid where a flow helps, Zensical `!!!` admonitions, paragraphs <= 2 sentences, zero `ADR-*`/milestone lawyer prose; every documented command must exist in the shipped binary (`tools/check_docs_honesty.py`).

# Acceptance
- `bash -n docs/install.sh && dash -n docs/install.sh`
- `make packages-bundle && scripts/check-install.sh --bundle target/package-bundle` exit 0 (real file:// install into a throwaway prefix, no `--mirror`).
- `make docs-check` exit 0 and `uv run --no-project tools/check_docs_honesty.py` exit 0.
- `git grep -inE 'cloudsmith|forgejo|chulak|forge\.offworld\.nexus' -- . ':!plan'` is empty.
- `uv run --no-project tools/plan.py validate && uv run --no-project tools/guard.py` exit 0.
