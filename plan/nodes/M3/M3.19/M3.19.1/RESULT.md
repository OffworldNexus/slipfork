# Result M3.19.1 — H-006 AppArmor attachment set applied

## Ruling applied (architect H-006, `ADR-0010` §7)

The `/etc/apparmor.d/slipfork` profile must attach **every** executable that performs
`CLONE_NEWUSER` for a world, not only `<bin_dir>/slipfork{,d}`. Ubuntu 24.04 with
`kernel.apparmor_restrict_unprivileged_userns=1` transitions the in-world `/.slipfork/bin/crun`
into `unprivileged_userns` on `execve` and denies `CAP_SYS_ADMIN`, so every world boot died with
`unshare: Operation not permitted` (`plan/nodes/M3/M3.19/RESULT.md` §2.3/§10). AppArmor 4.0 accepts
one attachment expression per profile header, so all paths live in one leading-slash brace
expression. No fallback, feature-detect branch, or second profile shape was added.

## Exact rendered profiles

Product (`cargo run -q -p slipfork -- daemon unit --only apparmor`, exit 0):

```text
abi <abi/4.0>,
profile slipfork /{home/remy/offworld-nexus/slipfork/target/debug/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} flags=(unconfined) {
  userns,
}
```

Harness (`tools/do_vm.py::render_apparmor_body("/home/slipfork/slipfork")`):

```text
abi <abi/4.0>,
profile slipfork /{usr/local/bin/slipfork{,d,-world-init},home/slipfork/slipfork/target/{debug,release}/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} flags=(unconfined) {
  userns,
}
```

`<bin_dir>` is stripped of leading/trailing `/`; `bin_dir == "/"` renders the bare
`slipfork{,d,-world-init}` clause, so no `//` sequence can appear.

## Code changes

| File | Change |
|---|---|
| `crates/slipfork/src/apparmor.rs` | `render_userns_profile` emits one `/{<rel_bin_dir>/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun}` expression (bare bin clause at `/`); module docs now name all five executables |
| `crates/slipfork-engine/src/lib.rs` | `userns_remediation_hint` names the same full attachment set; still names `slipfork daemon unit --only apparmor` and `sudo apparmor_parser -r /etc/apparmor.d/slipfork`, and no longer implies the entry binary alone suffices |
| `tools/do_vm.py` | `render_apparmor_body` emits the harness clause with `rel = remote_tree.strip("/")` and the byte-identical `,usr/bin/unshare,.slipfork/bin/crun}` tail |
| `tools/tests/test_do_vm.py` | exact 4-line assertions for both renderers; byte-equality after normalising **only** the `<bin_dir>` clause to `<bin_clause>`; no-host-`crun` test scoped to `PACKAGES`/the `apt-get install` line |
| `docs/reference/cli.md` | shows the rendered 4-line profile verbatim and keeps the two install commands; no claim that only `slipfork{,d}` is attached |

`crates/slipfork/src/doctor.rs` needed no edit: it already delegates to
`userns_remediation_hint` and its doc comment ("names the exact profile attachment") stays true.

## Unit tests added

`crates/slipfork/src/apparmor.rs` (5 tests, all green):
- exact expected string for `/home/dev/.local/bin`;
- `bin_dir == "/"` renders a bare clause with no `//`;
- no `//` for `/`, `/usr/bin`, a real bin dir, and a trailing-slash bin dir;
- one brace expression expands `{,d,-world-init}` to `slipfork`, `slipforkd`,
  `slipfork-world-init` and also names `usr/bin/unshare` and `.slipfork/bin/crun`;
- no file/mount rules.

`crates/slipfork-engine/src/lib.rs` (2 tests, green): the hint names the full attachment set plus
both install/reload commands (and the bare `/` clause), with no `//`.

`tools/tests/test_do_vm.py`: `test_apparmor_body_renders_the_ruling_exactly`,
`test_cli_rendered_apparmor_profile_matches_the_harness_skeleton` (rewritten normalisation), and
`test_provision_script_installs_no_host_crun_and_keeps_git` (scoped to `PACKAGES`/install line).

## Acceptance

| Command | Exit | Evidence |
|---|---|---|
| `cargo test -p slipfork --lib apparmor` | 0 | 5 passed, 0 failed |
| `cargo run -q -p slipfork -- daemon unit --only apparmor` | 0 | 4 lines above |
| `! grep -n '//' <(cargo run -q -p slipfork -- daemon unit --only apparmor)` | 0 | no `//` |
| `cargo test -p slipfork-engine --lib` (extra) | 0 | 64 passed |
| `cargo test -p slipfork --lib` (extra) | 0 | 34 passed |
| `uv run --no-project --with pytest pytest -q tools/tests/test_do_vm.py` | 0 | 60 passed |
| `uvx --offline ruff check tools && uvx --offline ruff format --check tools` | 0 | all checks passed, 12 files formatted |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | clean |
| `cargo fmt --all -- --check` | 0 | clean |
| `make docs-check` | 0 | zensical strict build OK; `check_docs_honesty: OK` |
| `uv run --no-project tools/guard.py` | 0 | 503 files checked |
| `uv run --no-project tools/plan.py validate` | 1 | **pre-existing plan metadata**: `M3.19: task has children; convert with 'edit M3.19 --kind epic' first` — `M3.19` is still `kind: task` while owning `M3.19.1`; outside this node's paths (lead action) |

`make check` / `make rs-test` were intentionally not run (serial gate owned by the lead).

## Next step (not done here)

Live re-verification on `165.232.86.174` — remove `/var/lib/slipfork/.slipfork-ready`, run
`tools/do_vm.py up --yes`, `tools/do_vm.py sync`, `tools/do_vm.py test`, reinstall
`/usr/local/bin/{slipfork,slipforkd,git-remote-slipfork,slipfork-world-init}`, and verify
`/home/slipfork/demo` (`slipfork up`, `slipfork fork`, `slipfork exec -- crun --version`) — is the
next step, not done here.
