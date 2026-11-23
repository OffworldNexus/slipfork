# M2.23 RESULT — Purge stale M1 manifest/snow-mode docs

STATUS: done

## What changed

| Page | Change |
|---|---|
| `docs/reference/manifest.md` | Deleted `OsppBundle`/REST bundle/`TopLevelEnvsUnsupported`/`LocalHostRoute::url()` prose. One canonical `ForbiddenTopLevelKey { key }` statement (key only, real stderr quoted once), one `effective_components` table (`run_command` override/mode-only add/`disabled: true`), expressions table (namespaces, `??`, `!!`-only-on-`secrets`), `ResourceCoordinates::for_world` table (`app_url` = single `WorldOrigin` URL, portless at `:80`), `LocalHostRoute::parse`, image variants, `[resources]` parses-now note, one-sentence `OSPP v1` `EnvBundle` pointer (`ADR-0008` §2) with the local vault as the only provider read today. 155 lines. |
| `docs/snow-modes.md` | Retired bundle format and old component rule deleted. Example now parses (`mailpit` prebuilt image, `disabled: true` in `cloud`), mode-resolution table, component add/override/remove table (`modes.all.components` refused), `!!` vs `??` rules (`??` needs a JSON string literal; generator encodings), fail-closed example with exit `1` and persisted-`!!` caveat, single-origin coordinates (origin port vs container ports, `domains::` scoped to today + `ADR-0005` §1). 118 lines. |
| `docs/reference/cli.md` | Only real commands documented (`doctor`, `snow resolve`, `dag plan`, `snap`) with `--json` payload table, exit-code table, `--file` CWD rule, `--component` failure text. `fork`/`exec`/`drop` moved to a "Daemon-Owned Commands (`0.2.0`)" section with the exact diagnostic and flag defaults; no runnable example. |
| `docs/getting-started.md` | M1 fork/exec/drop quick-start replaced by four runnable steps (build+doctor, resolve, `dag plan`, `snap`), copy-pasteable `./pgdata` setup, Rust 1.94+, bare-repo correction, `Rebuild`-only cache truth, daemon note. |
| `docs/index.md` | Working 4-command example, world-lifecycle warning with the exact reason, "What Runs Today" table, M1 benchmark labelled as a committed report, roadmap ref fixed to `refs/heads/<fork>` + `refs/slipfork/<fork>/wip`. |
| `docs/agents-and-opencode.md` | Honest status first (do not enable against a `0.1.0` binary; fork refusal rolls the worktree back), V2-correct plugin entry (`{ "package": "../packages/opencode-slipfork", "options": … }`, config-file-relative, directory not file), hooks table, roadmap. |
| `README.md` | Same status/quickstart truth as the site, image-agnostic benchmark wording, layout table trimmed, `make check` gate, corrected `refs/slipfork/<fork>/wip`, `target-dx.md` labelled as unshipped internal spec. |
| `docs/design/m2-manifest-docs.review-dx.md` | New critic/applied-findings record (dx + ai-ergonomics + cruft lenses). |

## Transitional diagnostic wording used

Verbatim (exit `2`), quoted once per site page as `text` (never as a runnable example):

```text
slipfork fork is served by slipforkd in 0.2.0 (lands in M2.17); world 'exp' is daemon-owned and no cross-process world handle exists — run `slipfork doctor` to verify this host
```

Pages state: the three commands "exist and parse their flags (invalid flags exit `1`), then exit `2`"; `exec`/`drop` produce the same sentence with their own name; `--json` emits `{"ok": false, "error": …}` on stdout.

## Acceptance evidence

```console
$ ! rg -q 'OsppBundle|from_bundle|TopLevelEnvsUnsupported|LocalHostRoute::url' docs/reference/manifest.md docs/snow-modes.md
exit=0
$ rg -q 'disabled: true' docs/reference/manifest.md
exit=0
$ rg -q 'ADR-0008' docs/reference/manifest.md
exit=0
$ wc -l < docs/reference/manifest.md
155
$ wc -l < docs/snow-modes.md
118
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)
exit=0
$ make docs-check
Build started
No issues found
Build finished in 0.18s
uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)
exit=0
$ make check
make-check-exit=0
```

Also verified by running the published examples: `snow-modes.md` and `manifest.md` YAML blocks resolve (`snow resolve --mode local`, exit `0`); `dag plan`/`snap` outputs were captured from the real binary; top-level `envs:` and `--workspace /nope` exit `1` with the quoted text.

## Follow-ups (outside this node's paths)

1. **`poc/run_benchmark.py` still drives M1 `fork`/`exec`/`drop`**, so `make bench` cannot reproduce the published numbers until `M2.15` aligns it with the `0.2.0` CLI. Docs cite the committed report by path only.
2. **Version labels** (`0.1.0` in page titles and the `zensical.toml` nav) must move with the single `0.2.0` version bump (`M2.20`).
3. **The standalone `dx` critic did not return a final report** before close-out; the dx lens was applied by the author and the fresh-context `ai-ergonomics` + `cruft` findings were resolved (see the review record).
4. `crates/slipfork-dag`'s `[resources]` strategies are parsed but consumed only by `slipforkd` (`0.2.0`); the reference page now says so instead of omitting them.
