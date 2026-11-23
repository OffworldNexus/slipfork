---
id: M1.8
title: "Fold owner review-gate corrections: single-origin invariant, support tiers, typed coverage (no shell recipes)"
kind: task
status: done
owner: architect
priority: 1
depends_on: [M1.7]
paths:
  - ARCHITECTURE.md
  - "docs/decisions/**"
  - "docs/design/**"
  - docs/VISION.md
  - docs/index.md
  - docs/reference/manifest.md
  - README.md
  - CHANGELOG.md
  - plan/ROADMAP.md
attempts: 2
max_attempts: 3
escalation: architect
created: "2026-11-15T10:38"
updated: "2026-11-15T11:11"
---

# Goal

Fold four owner decisions (given at the `0.1.0` release gate) into the design of record, **before** `0.1.0` merges, so we do not ship an ADR the owner has already rejected. Docs/ADRs only — no code changes in this node.

## 1. Single canonical origin per world (amend `ADR-0005`)

**Invariant: a world has at most one public origin.** `WorldOrigin = Local { url } | Shared { url }` — never both, never two.

Consequences that must be stated normatively in `ADR-0005` + `ARCHITECTURE.md`:

- The origin is assigned at **world materialization**, never sealed into a snapshot. Therefore `${{ resources::app.url }}`, `${{ domains::main }}`, `BASE_URL`, `CSRF_TRUSTED_ORIGINS`, cookie domain, OAuth redirect URIs, email absolute links, `AWS_S3_CUSTOM_DOMAIN`, and any `django.contrib.sites` / Wagtail `Site.root_url` row are **per-world values resolved at boot**, and MUST NOT be baked into a reusable fixture snapshot.
- Sharpen the DAG contract accordingly: a `[fixtures.<id>]` node is either **origin-independent** (migrations, seeds that read only `DATABASE_URL`-class vars) or **`per_world = true`** (it writes absolute URLs into persistent state; it can never be shared across worlds and runs after origin assignment). Delete any prose implying all fixture snapshots are always shareable.
- `slipforkd` behaviour when the requested world is shared: `http://<anything>.<project>.localhost[:80]` answers **`308 Permanent Redirect`** to the world's single shared origin for every method (method + body preserved, `Expect: 100-continue` handled before body read). For requests that cannot be redirected by a client (HTTP `Upgrade` / WebSocket), the daemon must **proxy-pass** to the shared origin and rewrite `Host` + `X-Forwarded-*`, or refuse with an actionable error naming the shared URL — pick one and say so explicitly.
- Sharing is **fixed at world materialization**: `share`/`unshare` cannot mutate a live world's origin (that would leave stale env in already-started processes). Changing sharedness re-creates the world from its snapshot and re-assigns the origin. State this as the reason, not as a limitation.
- `StaticUrl`/S3 signing, absolute URLs in emails, and OAuth callbacks all follow from the one origin; the world card / `slipfork list` must print exactly one URL per world.

## 2. Integration support tiers (new `ADR-0007`)

We promise many platforms and frameworks; the docs must say **how well** each is actually integrated, with no room to overclaim. Define a small ordered tier set, each with **machine-checkable evidence**:

| Tier | Meaning | Evidence the repo must contain |
|---|---|---|
| `none` | No meaningful test surface; nothing to integrate (e.g. a pure CLI library) | written rationale + reviewer sign-off |
| `declared` | Manifest recipe written, never exercised outside its author | recipe page + `critical_*` parse test |
| `exercised` | CI runs a real project through it | named CI job + a passing run |
| `field-tested` | A real team/product runs its suite through it | named deployment reference |
| `hardened` | Field-tested at scale, including browser/multi-worker/flaky-suite behaviour | production suite reference + test audit |

- One source of truth (a registry file), a `support` CLI view (`--json` included), and a **generated** docs table so the site cannot drift from reality.
- Promotion is a plan node; **demotion is automatic** when the referenced evidence disappears (deleted test/job), enforced in `make check`.
- This replaces any ad-hoc "supported frameworks" claim in `VISION.md`/`README.md`.

## 3. Coverage is built in; delete the shell interpolation (amend `ADR-0006`)

Owner rejected `[test].after_suite` + `$SLIPFORK_ARTIFACTS_DIR` shell recipes ("shell interpolation", "building up the path manually"). **Delete that user-facing surface entirely.**

- Replace with a typed intent:
  ```toml
  [test]
  coverage = true
  coverage_report = ["html", "xml"]
  coverage_fail_under = 85        # optional
  ```
- `TestAdapter` gains a typed coverage contract (capability + world env + collect). **The adapter owns file naming, temp paths, and combination; the user never writes a path or a pipeline.** Per language: Python `COVERAGE_FILE` + `--cov` / combine; Node `NODE_V8_COVERAGE`; PHP harness-supplied clover path; Rust `LLVM_PROFILE_FILE`. Combination across N worlds happens in the orchestrator, not a shell.
- Add the general design law this implies (state it once, in `ADR-0006`): **Slipfork configuration never contains shell pipelines, manual path assembly, or env interpolation for values the tool itself owns — if a value is needed it is a typed field, if a path is needed the tool supplies it.**
- Blastit / Allure / Playwright-traces orthogonality survives without user-authored shell: the tool passes the world a documented directory via an env var **the tool owns and documents**, and the suite's own tooling writes there; Slipfork only ensures it survives teardown.
- Record that `after_suite` never ships (zero backward compatibility, Iron Law #3), and scrub every mention from `ARCHITECTURE.md`, `docs/design/*`, `docs/decisions/*`, `CHANGELOG.md`.

## 4. Keep what the owner approved (do not regress)

Mode-scoped components (`modes.<mode>.components.<id>` add/remove/override) and test-scoped hooks (`[test]`) are **approved as designed** — make sure the amended docs say so explicitly and do not weaken them while editing.

# Acceptance

- [ ] `rg -n "after_suite|SLIPFORK_ARTIFACTS_DIR" ARCHITECTURE.md docs CHANGELOG.md` exits 1 (0 hits).
- [ ] `rg -n "single origin|at most one (public )?origin|308" docs/decisions/ADR-0005*` exits 0 and the redirect rule is normative (not prose).
- [ ] `test -f docs/decisions/ADR-0007*` exits 0 and the tier table carries a required-evidence column.
- [ ] `uvx --offline zensical build --strict` exits 0 and `uv run --no-project tools/check_docs_honesty.py` exits 0.
- [ ] `make check` exits 0.
