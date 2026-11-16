# M3.5 RESULT — DigitalOcean dev-VM harness (`tools/do_vm.py`)

- **Attempt:** 1/3
- **Date:** 2026-11-16
- **Owner overrides applied:** default region `ams3` for droplet + volume; default size `s-4vcpu-8gb`; default volume `100 GiB`; cost strings use ams3 + `s-4vcpu-8gb`.
- **Owner addendum applied:** create-time embedding of **all** visible SSH key ids, repeatable `--ssh-key` restriction, repeatable `--authorized-key` post-boot append, `status` fingerprint probing, docs/tests for the DO registry quirk.
- **Real cloud resources created:** **none.** Every verification used fake `doctl`/`ssh` executables on `PATH`.

## Delivered surface (exactly six commands)

```text
tools/do_vm.py up      [--region ams3] [--size s-4vcpu-8gb] [--volume-gb 100]
                       [--ssh-key NAME_OR_ID ...] [--authorized-key PATH ...] [--yes]
tools/do_vm.py status
tools/do_vm.py sync
tools/do_vm.py test    [-- <extra cargo args>]
tools/do_vm.py ssh
tools/do_vm.py down    [--yes]
```

- `up` find-or-creates the tagged droplet/volume **independently**: droplet-only creates + attaches the volume (`volume-action attach --wait`, only if `volume_ids` lacks it) and re-provisions over SSH; volume-only creates the droplet with `--volumes`; a complete attached pair is a no-op report. A second run creates/attaches nothing.
- Cloud-init is one `runcmd` chain (`bash -lc`, `set -euo pipefail`): `apt-get update` -> `zfsutils-linux` + `incus` (prints exact `apt-cache policy incus` and stops when unavailable; no PPA/zabbly) -> `zpool create -o ashift=12 -O mountpoint=/var/lib/incus/disks/pool incuspool /dev/disk/by-id/scsi-0DO_Volume_*` -> `systemctl enable --now incus` -> bounded `incus info` poll -> `incus storage create default zfs source=incuspool` -> `/root/incus-ready`. Pool/storage steps are existence-guarded so re-provisioning converges.
- `sync` tar-pipes tracked + uncommitted non-ignored files via Python `tarfile` over SSH, prunes `target/ site/ .worktrees/`, and refuses `.env`/`.env.*` (allows `.env.example`) before any SSH call.
- `test` pre-checks the local and remote `crates/slipfork-engine/tests/critical_incus_live.rs`, fails closed naming `critical_incus_live` + `M3.3`, checks `cargo`, then streams `cargo test -p slipfork-engine --test critical_incus_live -- --test-threads=1` plus forwarded args.
- `down` is `--yes`-guarded, tag-scoped, droplet first then volume, and succeeds with only one resource present.
- Docs: `docs/design/do-vm-harness.md` (internal; excluded from Zensical) documents commands, cost model, ZFS/CoW rationale, LXC-only/no-nested-KVM limitation, the DO SSH-key registry quirk, and the explicit “test harness, not a supported deployment path” non-goal.

## Acceptance evidence

### `uv run --no-project tools/do_vm.py --help` — exit 0, six commands

```text
usage: tools/do_vm.py [-h] command ...

positional arguments:
  command
    up        create the tagged droplet + ZFS volume and wait for Incus
    status    report tagged droplet and volume state
    sync      tar-pipe the working tree to /root/slipfork
    test      stream the live IncusEngine conformance suite
    ssh       open an interactive root SSH session
    down      destroy the tagged droplet and volume
EXIT=0
```

### `uv run --no-project tools/do_vm.py up --help` — exit 0, required flags

```text
usage: tools/do_vm.py up [-h] [--region REGION] [--size SIZE]
                         [--volume-gb VOLUME_GB] [--ssh-key NAME_OR_ID]
                         [--authorized-key PATH] [--yes]
  --region REGION       DigitalOcean region (default: ams3)
  --size SIZE           droplet size slug (default: s-4vcpu-8gb)
  --volume-gb VOLUME_GB ... (default: 100)
  --ssh-key NAME_OR_ID  restrict the droplet to this visible key (repeatable);
                        default: every visible key
  --authorized-key PATH ... (repeatable; idempotent via sort -u)
  --yes                 skip the cost confirmation prompt
EXIT=0
```

### `uv run --no-project tools/do_vm.py test` — non-zero, names the missing suite

```text
error: missing live Incus conformance suite crates/slipfork-engine/tests/critical_incus_live.rs (delivered by M3.3); `critical_incus_live` cannot run yet
EXIT=2
```

No `doctl`/`ssh` call is made before this local pre-check.

### `uv run --no-project --with pytest pytest -q tools/tests` — exit 0

```text
...................................................................      [100%]
67 passed in 3.12s
```

`tools/tests/test_do_vm.py`: 26 fake-`doctl`/fake-`ssh` tests, including:

- `test_up_defaults_and_argv_construct_ams3_s_vm` — `--region ams3`, `--size s-4vcpu-8gb`, `--size 100GiB`, `--tag slipfork-dev-vm`, `--volumes vol-123`, `$58.00/month`; cloud-init contains `apt-cache policy incus`, `zpool create -o ashift=12`, `incus storage create default zfs source=incuspool`, no `zabbly`.
- `test_up_reuses_tagged_droplet_and_volume` — second `up` creates nothing and makes no `volume-action attach` call (already-attached case).
- `test_up_reconciles_droplet_only_by_creating_and_attaching_the_volume` — creates the missing volume, attaches it once, never creates a second droplet; second run does neither.
- `test_up_reconciles_volume_only_by_creating_the_droplet_with_it` — creates the droplet with `--volumes vol-123`, no volume create, no separate attach; second run creates nothing.
- `test_up_fails_closed_with_zero_visible_keys`, `test_ssh_key_restriction_resolves_name_and_id`, `test_ssh_key_unknown_fails_closed` — all-visible-keys default, repeatable restriction, unknown rejected.
- `test_authorized_key_is_appended_idempotently_and_fingerprints_printed` — content goes over SSH stdin (never argv/log), second run leaves `authorized_keys` byte-identical (one occurrence), fingerprints printed.
- `test_status_prints_fingerprints_and_fails_closed_when_probe_fails` — prints fingerprints; non-zero when the SSH probe fails.
- `test_down_succeeds_with_only_a_droplet`, `test_down_succeeds_with_only_a_volume`, `test_down_deletes_droplet_before_volume_and_never_without_yes` — partial teardown + ordering + `--yes` guard.
- `test_sync_refuses_env_files_before_any_ssh`, `test_sync_tar_pipes_working_tree_and_allows_env_example`, `test_sync_does_not_refuse_env_files_excluded_from_the_tar` — `.env` refusal before SSH; `.env.example` synced; an ignored `vendor/.env` never tarred does not block.
- `test_test_command_names_missing_suite_and_m3_3`, `test_test_streams_suite_and_forwards_extra_cargo_args` — missing-suite message; `-- --nocapture` forwarded after `--test-threads=1`.
- `test_up_fails_fast_on_publickey_denied` — `Permission denied (publickey)` names `--ssh-key`/`--authorized-key` immediately, never reports cloud-init timeout.
- `test_up_prints_summary_before_fingerprints_and_never_deletes` — summary (`down --yes`, cost, SSH) precedes the fingerprint probe; fingerprint failure still prints `down --yes`; no `delete` action is ever issued.
- `test_status_malformed_droplet_networks_fails_closed` — malformed `networks.v4` raises `HarnessError`, not `TypeError`/traceback.
- `test_ssh_command_allows_interactive_auth` — `ssh` omits `BatchMode=yes`; probes keep it.
- `test_provision_script_is_valid_bash`, `test_provision_script_fails_closed_when_incus_candidate_is_none` — single-sourced script passes `bash -n`; stub `apt-cache` `Candidate: (none)` exits 1 after printing the policy.

### `rg -n "dogeops|DogeOps|60390733" tools/do_vm.py` — exit 1 (no account literals)

```text
RG_EXIT=1
```

### `uvx --offline ruff check tools && uvx --offline ruff format --check tools` — exit 0

```text
All checks passed!
10 files already formatted
EXIT=0
```

### `uv run --no-project tools/guard.py` — exit 0

```text
guard: OK (394 file(s) checked)
```

### `make check` — exit 0

Round 2 rerun (all suites green):

```text
$ make check
uv run --no-project tools/plan.py validate
plan OK
uv run --no-project tools/guard.py
guard: OK (394 file(s) checked)
uvx --offline ruff check tools poc
All checks passed!
uvx --offline ruff format --check tools poc
11 files already formatted
uv run --no-project --with pytest pytest -q tools/tests
67 passed in ~3s
...
cargo test --workspace --all-targets
... (all suites green)
uvx --offline zensical build --strict
No issues found
uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)
MAKE_EXIT=0
```

Round 1 had hit the conductor-recorded pre-existing `critical_oci_runtime` world-boot timing flake in a default-thread run; every round-2 rerun was green.

## Round 2 gate fixes applied (reviewer / legality / ioc / cruft)

1. `_wait_ready` now fails immediately on `Permission denied (publickey)` / `Too many authentication failures`, naming `--ssh-key` and `--authorized-key`; retries are limited to connection-refused/not-yet-up/transient transport markers. `_wait_ssh` applies the same rule before re-provisioning.
2. Billing safety: after `/root/incus-ready`, `--authorized-key` files are installed, then the summary (IP, `ssh root@<ip>`, cost, exact `down --yes`) prints **before** the fingerprint probe. A fingerprint-probe failure (or authorized-key install failure) still includes the exact `down --yes` command; `up` never issues `delete`.
3. `sync` refuses `.env`-class files only from the exact `--exclude-standard` list it tars; an ignored `vendor/.env` never sent does not block.
4. The create line prints `(restricted by --ssh-key)` or `(all keys visible to this doctl context)` as appropriate.
5. `ssh_argv(..., batch=False)` for `cmd_ssh` so an interactive passphrase-protected key can prompt; all probes stay `BatchMode=yes`.
6. `networks.v4` and volume `tags` are shape-validated with `HarnessError` (no `TypeError`/`AttributeError`).
7. `PROVISION_SCRIPT` is single-sourced; tests run `bash -n` and a stub `apt-cache` `Candidate: (none)` fail-closed path.
8. Existing tagged resources print: `note: existing tagged droplet/volume win; --size and --volume-gb apply only to newly created resources`.
9. Removed control-flow `assert`s; collapsed readiness onto `_ready_probe()`; use `TAG` directly; added `_at_most_one(label, items)` with the single remediation message.
10. Docs: corrected the sync wording, marked the volume price as list-price-at-time-of-writing, and phrased the DO 422 behaviour as observed.
11. `up` prints `prereq   install Rust >= 1.85 (rustup) ... cloud-init provisions ZFS + Incus only`; the runbook documents it.

## Final polish pass applied

- `_wait_ssh` and `_wait_ready` now share one `_poll(...)` retry ladder (probe -> `_ssh_failure` -> sleep -> bounded timeout). Only auth/host-key errors abort early; every other SSH failure keeps polling and the last probe detail is reported in the timeout message. The cloud-init log dump lives only in the `_wait_ready` wrapper. Timeouts unchanged (`5 min` / `15 min`). `SSH_TRANSIENT` was deleted as no longer serving a purpose.
- `_single_droplet` reuses `_at_most_one("droplet", ...)`; create reads use `_exactly_one("volume"|"droplet", ...)`; one `_detail(probe)` helper replaced the repeated `(stderr or stdout or "").strip()`.
- `cmd_test` calls `_ssh_failure` on rc 255 first, so auth rejection surfaces `--ssh-key`/`--authorized-key` instead of "cannot reach".
- `Doctl.regions()` validates every list entry is a dict before `item.get(...)`; malformed payloads raise `HarnessError`, never `AttributeError`.
- Tests: help asserts the parsed subcommand set exactly; the fail-closed apt test stubs `systemctl`/`zpool`/`incus`/`journalctl` in the same PATH and asserts they are never invoked.
- Price constant is date-stamped `as of 2026-11-16`; docs §3 mirrors the date. Docs now say `.env` refusal applies only to files selected for transfer, document fast-fail SSH auth and the bounded SSH-reachability gate before re-provisioning, remove duplicated Rust/`--authorized-key`/cost-example text, and keep the "not a supported deployment path" non-goal only in §6.

## Deferred (record only; delete when picked up)

- Unquoted remote-command interpolation is safe because every operand is a module constant (`REMOTE_ROOT`, `AUTHORIZED_KEYS`, `SUITE_PATH`) or `shlex.quote`-escaped; the invariant is documented in `ssh_argv`.
- Tar-stderr threading.
- `Doctl.run` mutation-adapter consolidation.
- Sharing `git ls-files` enumeration with `guard.py`.
- `UpPlan` state-machine extraction.
- `SshKey` dataclass.
- `--size`-unknown cost path.

## Constraint notes

- DigitalOcean droplets expose no nested KVM, so the docs and harness describe **Incus system containers (LXC) only**; no QEMU/stateful-RAM claim anywhere.
- `test` requires a Rust toolchain ≥ 1.85 (edition 2024) on the VM; cloud-init provisions ZFS + Incus only, and `test` fails closed with that message when `cargo` is absent.
- `up` never destroys. Teardown is always the explicit `tools/do_vm.py down --yes`.
- `--authorized-key` key material is read from the runtime path, streamed over SSH stdin, and never embedded in argv, logs, cloud-init, tests, or docs.

## Files

- `tools/do_vm.py` (new)
- `tools/tests/test_do_vm.py` (new)
- `docs/design/do-vm-harness.md` (new)
- `Makefile` — no change required; `py-lint`/`py-test` already cover the new tool and tests.

## Follow-ups

- `M3.3` must land `crates/slipfork-engine/tests/critical_incus_live.rs`; `tools/do_vm.py test` is already wired to stream it.
- The conductor should run `up --authorized-key <path> --yes` and verify the printed fingerprints with the owner before logging in.
