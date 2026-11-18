# M3.28.23 — Attributed logs

Base `c5dda37` (`release/0.1.0`). No commit was made by this node.

## What changed

The label is **data from `slipforkd`** end to end: engine log chunk → edge frame → daemon fan-out →
CLI line buffer. Nothing in the renderer guesses a source.

- `crates/slipfork-engine/src/lib.rs` — new `LogChunk { target, source, bytes }` and
  `LogChunk::attribute(target, StreamChunk)`: the attributed chunk of a **retained** log.
  `StreamChunk` stays the un-attributed stream a live `exec` / `hook run` yields (those have no
  retained target), so the two facts never share a type.
- `crates/slipfork-engine/src/service.rs` — `ServiceLogs` records the id it was minted for
  (`ServiceLogs::new(spec.id)`, `ServiceLogs::target()`), so a service chunk is labelled from the
  engine's own record, never from the caller's `LogsRequest.target` string. Retained
  `snapshot()` / `follow()` are unchanged (`Vec<StreamChunk>`); Debug reports the target.
- `crates/slipfork-engine/src/btrfs_engine.rs` — the three `ServiceLogs::new` sites pass the
  service/hook id (`spec.id`, the test world's `id`, `test_spec().id`).
- `crates/slipfork-edge/src/daemon.rs` — `DaemonControl::logs` streams `LogChunk`s, and the wire has
  its own frame: `DaemonReply::Log { log }`. `DaemonReply::Chunk { chunk: StreamChunk }` remains the
  `hook run` frame (a live child stream with no retained target), so the two are not a dual mode.
- `crates/slipfork-edge/src/daemon/wire.rs` — `write_log` splits an over-cap `LogChunk` and copies
  `target` + `source` onto **every** piece: a split chunk is still attributable.
- `crates/slipfork-edge/src/daemon/{server,client}.rs` — `stream_logs` / `read_terminal` dispatch
  `Log` frames to the log callback; `logs` is the only caller that passes one.
- `crates/slipfork/src/daemon.rs` — the fan-out now emits `LogChunk::attribute(label, chunk)` per
  retained target; `HookLog` records the hook id it was minted for (`HookLog::new(id, path)`), so
  `api:pre-deploy` chunks are labelled from the hook record.
- `crates/slipfork/src/logs.rs` (new) — the one renderer. Fan-out (`target: None`) prefixes every
  line with `<source>: `; one named target writes bytes through untouched. Prefixing is
  line-buffered per `(target, stream)`: whole lines are emitted when their newline arrives, a line
  spanning chunks is prefixed exactly once, the trailing partial line is flushed by `finish()` at
  EOF. `--json` bypasses the line buffer and keeps the one-document-per-chunk NDJSON shape, now
  carrying the documented fields `{"stream", "source", "text"}` (`docs/design/dx-0.1.0-ux.md:360`).
- `crates/slipfork/src/main.rs` — `fn logs` is now request → `slipfork::logs::Renderer::new(json,
  request.target.is_none(), stdout, stderr)` → `client.logs(...)` → `finish()`. The old inline
  unprefixed fan-out is **deleted**, not kept behind a flag.

## The prefix rule (what "more than one source is selected" means at the CLI)

The CLI cannot know how many logs the daemon selected *before* the first chunk arrives — the wire
carries chunks, not a source census — and `docs/design/dx-0.1.0-ux.md` §8.3 defines the
multi-source case as "`slipfork logs` with no target". So the renderer's `prefix` flag is exactly
"the caller named no target": a fan-out request prefixes every line (correct attribution even when
the world happens to retain a single source), and `slipfork logs api` is byte-exact. `--json` is
unaffected by the flag.

## The wire change

```console
before:  {"reply":"chunk","chunk":{"source":"Stdout","bytes":[...]}}            # no target
after:   {"reply":"log","log":{"target":"api","source":"Stdout","bytes":[...]}}  # attributed
         {"reply":"chunk","chunk":{"source":"Stdout","bytes":[...]}}             # `hook run` only
```

`target` is the component id (`api`) or hook id (`api:pre-deploy`). An over-cap chunk is split on
the same `target`, so attribution survives `MAX_STREAM_CHUNK`.

## Two-source transcript (real `slipfork logs`, no target)

Two components (`api` and `worker`, each echoing its own line every 100 ms) in one world:

```console
$ slipfork up
$ slipfork logs --lines 40 | head
api: api-line
api: api-line
api: api-line
api: api-line
api: api-line
worker: worker-line
worker: worker-line
worker: worker-line
worker: worker-line
worker: worker-line
$ slipfork logs api --lines 3        # one named target: byte-exact, no prefix
api-line
api-line
api-line
```

`crates/slipfork/tests/critical_cli.rs::logs_fan_out_attributes_every_line_to_its_component` pins
both halves against a real `slipforkd` + `BtrfsEngine` world: every non-empty fan-out line starts
with `api: ` or `worker: `, and the named target contains the bare `api-line` and never
`api: api-line`.

## Acceptance (all run on the final tree)

| Command | rc | Result |
|---|---|---|
| `cargo test -p slipfork --lib logs` | 0 | `6 passed; 0 failed` (the 6 renderer tests: two sources, one named raw, split line, EOF flush, stderr routing, JSON) |
| `cargo test -p slipfork-engine --lib` | 0 | `69 passed; 0 failed` |
| `cargo test -p slipfork-edge --test critical_edge` | 0 | `35 passed; 0 failed` (the 20 KiB split test now exercises `write_log`) |
| `cargo test -p slipfork --test critical_cli -- --test-threads=1` | 0 | `30 passed; 0 failed` (29 before + the new attributed-logs test: fan-out prefixed, named target raw, `--json` `source`) |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | `Finished dev profile` |
| `cargo fmt --all -- --check` | 0 | clean |
| `uv run --no-project tools/guard.py` | 0 | `guard: OK (605 file(s) checked)` |
| `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| `uv run --no-project tools/critical_guard.py --base c5dda37 --node M3.28.23` | **1** | one unauthorized protected edit — see below |
| `make check` | 0 | full workspace gate (`plan-check guard stub-check py-lint py-test support-check ts-test rs-fmt rs-lint rs-test docs-check`) — `make check rc=0`; re-run on the frozen tree after two doc/test-only late edits (recorded in the node `LOG.md` by `lead`) |

Reproduce the renderer unit tests:

```console
$ cargo test -p slipfork --lib logs
test logs::tests::logs_a_line_split_across_chunks_is_prefixed_exactly_once ... ok
test logs::tests::logs_flushes_a_trailing_partial_line_at_eof ... ok
test logs::tests::logs_json_mode_is_the_unchanged_ndjson_document_per_chunk ... ok
test logs::tests::logs_one_named_source_writes_raw_bytes_unchanged ... ok
test logs::tests::logs_routes_stderr_lines_to_stderr_with_the_same_prefix ... ok
test logs::tests::logs_two_sources_prefix_every_line_with_its_own_source ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 99 filtered out
```

## One finding for the lead: `paths` gap (protected test + `main.rs`)

Invariant 1 cannot cross the daemon→CLI seam without `DaemonControl::logs`'s callback changing
type, and that trait's only other implementor is the fake in the protected
`crates/slipfork-edge/tests/critical_edge.rs`; the CLI's renderer call site lives in
`crates/slipfork/src/main.rs`. Neither is covered by this node's `paths` (whose third entry,
`"crates/slipfork/src/{daemon.rs"` + `logs.rs` + `"lib.rs}"`, is a broken brace glob that matches
nothing). The edits are mechanical, and **no assertion is weakened or deleted**:

- `crates/slipfork/src/main.rs` — the `logs` body becomes the renderer call (mechanical
  follow-through, as `M2.4`/`M3.28.10` did for `main.rs`).
- `crates/slipfork-edge/tests/critical_edge.rs` — the fake's `logs` signature now takes
  `&mut dyn FnMut(LogChunk)` and its one 20 KiB literal carries `target: "api"`; the frame-split
  assertions (`chunks.len() > 1`, 20 KiB total) are unchanged.

```console
$ uv run --no-project tools/critical_guard.py --base c5dda37 --node M3.28.23
critical-guard: c5dda37..working tree, 19 changed file(s)
  M  crates/slipfork-edge/tests/critical_edge.rs  [VIOLATION: not covered by any node path]
  M  crates/slipfork/tests/critical_cli.rs  [authorized by crates/slipfork/tests/**]
critical-guard: FAILED: 1 unauthorized protected test change(s) in M3.28.23.
```

Fix (lead-only authority), then re-run the guard:

```console
uv run --no-project tools/plan.py --agent lead edit M3.28.23 --paths \
  "crates/slipfork-engine/src/**,crates/slipfork-engine/tests/**,crates/slipfork-edge/src/**,crates/slipfork-edge/tests/critical_edge.rs,crates/slipfork/src/daemon.rs,crates/slipfork/src/logs.rs,crates/slipfork/src/lib.rs,crates/slipfork/src/main.rs,crates/slipfork/tests/**"
```

## Followup (outside this node)

- `slipfork logs --follow` with **no** target still follows only the first retained target: the
  daemon's fan-out iterates `retained` and calls `log.follow(...)`, which blocks until that log
  closes. This predates the node (attribution just makes it visible — `api:` streams, `worker:`
  never appears). Interleaving follow needs one thread per retained target feeding a channel that
  the request thread drains into `on_chunk`; that is a separate behaviour change, not part of
  invariants 1–5.
- The published `docs/reference/cli.md` `logs` rows (67, 105) do not yet state the `<source>: `
  prefix rule that `docs/design/dx-0.1.0-ux.md` §8.3 and this node implement; `docs/**` is outside
  this node's `paths`.

## Rule check

1. **Label is wire data** — `LogChunk.target` is set by the engine's `ServiceLogs::target()` (a
   service) or the daemon's `HookLog::target()` (a hook); the CLI only decides whether to *show*
   it.
2. **`--json`** — same NDJSON shape (one document per chunk, never line-split); it gains the
   documented `source` field, which the old line omitted even though
   `docs/design/dx-0.1.0-ux.md:360` already specified it.
3. **No new dependency, no global state** — `serde_json`/`serde` were already dependencies of all
   three crates; the renderer receives its two writers from `main.rs` (the composition root).
4. **No cruft** — `DaemonReply::Log` replaces the logs use of `Chunk` (which stays for `hook run`
   alone), and the old unprefixed CLI closure is deleted.
