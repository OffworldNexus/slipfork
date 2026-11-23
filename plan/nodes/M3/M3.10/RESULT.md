# M3.10 RESULT — `docs/design/do-vm-harness.md` (attempt 2/3)

**Status:** complete. Attempt 2/3. Only `docs/design/do-vm-harness.md` was edited (plus this
RESULT); no commit. Design of record for `M3.16`.

## Disposition of the 8 required fixes

| # | Requirement | Disposition |
|---|---|---|
| 1 | `blkid` must never coerce failure into "fresh volume" | Fixed §2.4: explicit `if out=$(blkid -p …); then rc=0; else rc=$?; fi` idiom; `0` non-empty → accept `btrfs` / fail on foreign, `0` empty → fail; `2` → only after an empty value **and** a bounded readability read (`dd … count=1`) run `mkfs.btrfs -f`; anything else (`4`, `8`, mid-detach) fails closed naming device, rc, stderr. No `|| true` anywhere. UUID capture uses the same idiom. `up`-never-destroys restated to match (only a proven-unformatted device is formatted). |
| 2 | One AppArmor renderer/owner + equivalence preflight + Cargo-resolved `slipfork-world-init` | Fixed §3: `slipfork daemon unit --only apparmor` is the sole normative body author (`M3.15` target state, labelled); the harness's embedded text is an explicitly interim copy that cloud-init must use (tree synced after `up`); the provisioning script is the only writer and rewrites only when absent/body-differs. `test` runs the built CLI renderer and fails closed on a divergent body or a pre-`M3.15` renderer; it never rewrites. `slipfork-world-init` is the `compiler-artifact.executable` for `-p slipfork-engine --bin slipfork-world-init`, required to be `test -x` and equal to the profile attachment, else exit non-zero. |
| 3 | Identity independent of product state | Fixed §1: owner-specified absolute `~/.cache/slipfork/do-vm/id_ed25519`, resolved from the operator's `$HOME` only; explicitly not `Layout::resolve`/`$XDG_CACHE_HOME`/`$XDG_DATA_HOME`/`--state-dir`/`--runtime-dir`; documented as harness-owned credential state in `do-vm/`, outside `worlds/`/`snapshots/`/`artifacts/`/`cache/`, never read/written/pruned by the product. `/tmp`-key-reboot lockout on a still-billing droplet stated. |
| 4 | No host `crun` | Fixed: removed from the apt list (§2.1), no host `crun --version` probe (§2.9), §6 and §1 `ssh` evidence now `slipfork shell -- crun --version` in-world; static pin ships in the `L0` rootfs (`ADR-0009` §1). |
| 5 | `M3.15` target-state label, world-init provability, DO automount citation | Fixed §3 (label + "only provable at the first live suite" because the tree arrives after `up`); §2.3 cites `docs.digitalocean.com/products/volumes/how-to/{create,mount-unmount}` (fetched 2026-11-16), states `up` creates the volume without `--fs-type` so nothing automounts, and marks the claim re-verify-on-first-`up`. |
| 6 | `--identity` per-subcommand semantics, `known_hosts` caveat, trailing command | Fixed §1 table: `up` repeatable (bootstrap + append all), `status`/`test`/`ssh` first-wins, `sync` no flag (SSH, default identity), `down` API-only/no SSH; per-worktree `known_hosts` caveat stated with the rationale for two homes; `ssh -- <cmd>` forwarding stated. |
| 7 | One `HOME` contract + real scratch roots | Fixed: `HOME=/var/lib/slipfork/home` for suites **and** the `docker info` preflight; `TMPDIR` on the volume; the three real roots named (`~/.cache/slipfork/test-tmp`, `~/.cache/slipfork/test-runtime`, `~/.cache/slipfork-cli-tests`). Added `/var/lib/slipfork/root-home` for owner `ssh` commands so their `slipfork` state is on Btrfs without colliding with the suite home. |
| 8 | Readiness marker inside the mount | Kept `/var/lib/slipfork/.slipfork-ready` and fixed the ordering bug: it is deleted **after** the mount is verified (not as the first script line), so an unmounted volume cannot expose a stale marker; matches the updated `M3.16` invariant. |

## Critic gate (5 lenses, inline findings; no review artifacts written — path lock)

| Lens | BLOCKER/MAJOR findings | Resolution |
|---|---|---|
| `ioc-upward` | AppArmor two-writer wording; `up` re-provision clobbers `test`-rendered profile; identity claimed "outside `<state_dir>`" (false for default cache); readiness `rm` before mount exposes stale marker; post-sync cargo/profile identity split; `ssh` root commands resolve `/root/.cache/slipfork` | All fixed: single writer with write-only-if-body-differs; re-render branch deleted; identity wording corrected (`do-vm/` is not product state); marker deleted after mount; build/render vs root-write split stated; `root-home` contract added |
| `legality` | `blkid` rc 2 ambiguity (formats unreadable device); DO automount trigger model wrong; "never destroys anything" false; teardown order backwards; `CARGO_BIN_EXE_*` claim false for CLI suite; empty subvolume assertion ignores sealed snapshots | All fixed: readability gate + rc 0-empty rule; automount model corrected (`--fs-type` omitted, create-time metadata); "never deletes/reformats" restated; `umount` first; artifact wording covers both suites; `grep -v '/snapshots/'` |
| `cruft` | Three AppArmor author stories; identity false claim; `up` re-provision contradiction; "re-run `up` cannot fix divergence"; duplicate AppArmor/HOME blocks; `CARGO_TARGET_DIR` re-render knob; two harness-state homes; `-z` is gzip; `58465342` unlabelled | All fixed: one ownership story; re-provision reconciled; divergence fix = correct both in one commit; §1.5 trimmed to summary, §4 points at §1; re-render branch deleted; known_hosts split justified; `tar --null -T -`; magic labelled |
| `implementability` | `blkid` rc 2 destructive; no user/env contract for post-sync cargo/build; `set -e` command-substitution pitfalls; fstab missing `nofail`; `journalctl -u cloud-init` matches nothing; passphrase keys hang; `env -i` omits `TMPDIR`; `git` missing; `aa-status` assertion vague | All fixed: readability gate; runuser+`env -i` prefix for builds; explicit status-capture idiom; `nofail`; `cloud-init.service`/`cloud-final.service`; `ssh-keygen -y -P ''` rejection; `TMPDIR` on volume; `git` in apt list; `aa-status | grep -q slipfork` |
| `testability` | `blkid` branch not executable with argv-only fake ssh; AppArmor unit cell tautological; no cells for renderer-absent/attachment-moved; live cells untagged; build argv missing from HOME cell; sync tar not asserted; automount only string-matched | All fixed: bash-executed `render_volume_signature_step()`/`render_automount_teardown()` with fakes; `compare_profile_body()` helper cell; renderer-absent cell; live evidence tagged `M3.19`; HOME/TMPDIR cell includes build; fake ssh consumes stdin; live teardown commands named |

## Acceptance evidence (this worktree)

```
$ ! grep -rniE 'zfs|incus|zpool|incus-ready' docs/design/do-vm-harness.md
exit=0 (no output)
$ grep -n 'mkfs.btrfs' docs/design/do-vm-harness.md
34, 329
$ grep -n 'user_subvol_rm_allowed,noatime' docs/design/do-vm-harness.md
341 (…,noatime,nofail)
$ grep -n -- '--identity' docs/design/do-vm-harness.md
23,24,26,27,37,59,124,…
$ grep -n 'apparmor' docs/design/do-vm-harness.md
12,192-198,296-303,379-434
$ grep -n 'critical_btrfs_live' docs/design/do-vm-harness.md
160,161,225,232
$ ! grep -riE 'option [ab]|spike [ab]' docs/design/do-vm-harness.md
exit=0 (no output)
$ make docs-check
zensical build --strict: No issues found
check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)
$ uv run --no-project tools/guard.py
guard: OK (483 file(s) checked)
$ uv run --no-project tools/plan.py validate
plan OK
```

## Cross-node followups for `lead` (outside `M3.10` `paths`)

1. `plan/nodes/M3/M3.10/NODE.md:19,29` still says `apt-get install … crun`; the doc and
   `M3.25/RESULT.md` followup #4 remove host `crun`. Needs a `plan.py edit`.
2. `plan/nodes/M3/M3.16/NODE.md:29` still says `doctor` reports `world_rootfs_crun`; `ADR-0010`
   §4 / `M3.25/RESULT.md` say zero host `crun` probe. Needs a `plan.py edit`.
3. `M3.16` `depends_on` is `[M3.10]`, but `test`'s AppArmor preflight requires the `M3.15`
   renderer. Either add `M3.15` to `depends_on` or accept the documented fail-closed message.
4. `M3.19` acceptance runs `ssh -- slipfork shell -- crun --version` and `slipfork prune
   --yes`; per §1 `ssh` these operate on `/var/lib/slipfork/root-home/.cache/slipfork` and need
   a world created there first (`slipfork up`). The node's acceptance sequence should add that
   step (or target an existing world) so the in-world `crun` evidence is meaningful.

## Attempt 3/3 — narrow dedupe pass (no behaviour, requirement, or claim changed)

Attempt-2 content accepted by the gate; this pass removed repeated prose only, plus three
legality-review MINORs. Only `docs/design/do-vm-harness.md` edited; no commit.

| # | Item | Disposition |
|---|---|---|
| D1 | AppArmor body-equivalence rule stated twice (§1 step 5, §3) | Kept once in §3 (rule, fail-closed-both-texts, no-rewrite, fix-both-in-one-commit-then-`up`); §1 step 5 is now one pointer sentence. |
| D2 | 7-line `runuser … env -i …` prefix pasted three times | Defined once as "the suite prefix" with a `<command>` slot; `fetch`, both builds, and the two suites are listed as trailing commands only. |
| D3 | "no host `crun`" asserted five times | One normative statement in §2 step 1 (apt list + `ADR-0009` §1 rationale); intro, §1 `test`, §2 step 9, and §6 are one-line pointers. |
| D4 | Marker-inside-mount / delete-after-verify restated four times | §2 step 7 is the single statement; §1 `up` step 6, §2 intro, §1 `test` step 1, and §2 step 11 point at it (step 11 keeps only the write-last fact). |
| D5 | `.pub`-derivation / fail-closed-before-create repeated in §5 | Deleted from §5; §5 now points at §1 `up` step 2 and keeps only the distinct "never regenerate a private key" rule. |
| D6 | Single-`HOME`/`docker info` rule repeated in §4 | §4 is the path table only; §1 `test` step 3 remains the single statement. |
| L1 | (legality MINOR) `blkid` rc 2 + successful `dd` proved readability, not emptiness | §2 step 4 now requires positive proof of emptiness before `mkfs.btrfs -f`: all-zero first MiB (`cmp -s <(head -c 1048576 "$dev") <(head -c 1048576 /dev/zero)`) or `wipefs -n` reporting no signatures; non-zero byte, unrecognized superblock, or bare partition table fails closed. §1 intro wording ("proven **empty**") and the §9 unit cells (fake `head`/`cmp`/`wipefs`, non-zero-first-MiB and signature cases) updated to match. |
| L2 | (legality MINOR) `slipfork-<sha256[:16]>` never said what is hashed | §5 now states the digest is SHA-256 of field 2 of `<path>.pub`. |
| L3 | (legality MINOR) identity table implied only `up` materializes | Added one sentence: `status`/`test`/`ssh` never materialize or regenerate the default identity; when absent they fail closed naming the path and pointing at `up`. |

### Acceptance evidence (attempt 3)

```
$ ! grep -rniE 'zfs|incus|zpool|incus-ready' docs/design/do-vm-harness.md   -> exit 0, no output
$ grep -n 'mkfs.btrfs' …                       -> 32, 319, 439, 544
$ grep -n 'user_subvol_rm_allowed,noatime' …   -> 334
$ grep -n -- '--identity' …                    -> 10 hits (21,22,24,25,35,57,125,464,557,558)
$ grep -n 'apparmor' …                         -> 14 hits (§2 step 1/9/10, §3, §9)
$ grep -n 'critical_btrfs_live' …              -> 161, 162, 221, 223
$ ! grep -riE 'option [ab]|spike [ab]' …       -> exit 0, no output
$ make docs-check                               -> zensical --strict: No issues found; honesty OK
$ uv run --no-project tools/guard.py            -> guard: OK (483 file(s) checked)
$ uv run --no-project tools/plan.py validate    -> plan OK
```

### IoC gate MINORs (folded into the same attempt-3 pass)

| # | Item | Disposition |
|---|---|---|
| I1 | "re-apply with `up`" not wired: `up` re-provisions only when the marker is absent | §3 preflight now gives the executable repair: correct both in one commit, `ssh -- rm -f /var/lib/slipfork/.slipfork-ready`, then re-run `up`, stating that a ready droplet is otherwise a no-op. No new flag, so the six-command surface of `NODE.md` is unchanged. |
| I2 | `sync` had no documented `HOME` contract | §1 `sync` now states its remote `tar`/`chown` run as `root` under `/root` by design, writing only `/home/slipfork/.sync` and `/home/slipfork/slipfork` and reading no `$HOME`-derived path — so there is no suite `HOME` to contract. |
| I3 | "harness-owned, never touched" rested on discipline inside the default state root | §1 identity ownership now says the exclusion is a rule, not a structural guarantee: any future state-root GC (`M3.2.13`) **MUST** exclude `do-vm/`, and names the structural alternative (a path outside `<state_dir>`) as the owner's unchosen option. |
| I4 | "never introduces a second profile body" contradicted the "interim copy" sentence | §3 now says the harness carries a **verbatim interim copy**, never a divergent second body. |
| I5 | `slipfork-world-init` wrongly called "not a hardcoded literal" | §3 bullet now calls it a **predicted default**, proven equal by `test -x` against the Cargo `compiler-artifact.executable`, with a mismatch failing closed naming both paths. |
| I6 | control `unshare` identity ambiguous | §1 `test` two-identity paragraph now states the control is launched by root but must execute as `slipfork` (`runuser -u slipfork -- unshare …`), because a root-exempt control proves nothing. |

### Acceptance evidence (final, after IoC MINORs)

```
$ ! grep -rniE 'zfs|incus|zpool|incus-ready' docs/design/do-vm-harness.md   -> exit 0, no output
$ grep -n 'mkfs.btrfs' …                       -> 32, 327, 450, 555
$ grep -n 'user_subvol_rm_allowed,noatime' …   -> 342
$ grep -n -- '--identity' …                    -> 10 hits
$ grep -n 'apparmor' …                         -> 12 sites (§2 steps 1/9/10, §3, §9)
$ grep -n 'critical_btrfs_live' …              -> 168, 169, 229, 231
$ ! grep -riE 'option [ab]|spike [ab]' …       -> exit 0, no output
$ make docs-check                               -> zensical --strict: No issues found; honesty OK
$ uv run --no-project tools/guard.py            -> guard: OK (483 file(s) checked)
$ uv run --no-project tools/plan.py validate    -> plan OK
```

### Functional-review MINORs (final addendum, same attempt-3 pass)

| # | Item | Disposition |
|---|---|---|
| F1 | `sync` named two mutually exclusive mechanisms | §1 `sync` now states one: archive built in-process with Python's `tarfile`, streamed over SSH stdin, extracted remotely with `ssh … 'tar -x -C /home/slipfork/.sync'`; the `tar --null -T -` clause is gone. |
| F2 | Interactive `ssh` claimed a `HOME`/cwd/PATH it cannot set | §1 `ssh` now specifies the wrapper `ssh -t … 'cd /home/slipfork/slipfork && exec env HOME=… TMPDIR=… PATH=… bash -l'`, with forwarded commands using the same envelope; the §9 row asserts both argv shapes. |
| F3 | `BatchMode=yes` attached to `ssh-keygen` | §1 `up` step 2 now says `ssh-keygen -y -P '' -f <path>` runs non-interactively and a non-zero status fails closed; `BatchMode=yes` remains an SSH-probe option only. |
| F4 | No target-state warning on the runbook | Header adds a Status line: target state, `tools/do_vm.py` rewritten by `M3.16`, shipped tool still the retired harness — do not run the runbook against today's binary. |
| F5 | "never introduces a second profile body" vs "interim copy" | §3 now reads: carries an explicitly interim copy of the authoritative body and introduces no second normative author. |
| F6 | DO automount footprint overstated as unit + udev + fstab | §2 step 3 now says `mnt-volume_*.mount` + udev rule on systemd images (Ubuntu 24.04), fstab route only for non-systemd images, keeping the "If one is present" hedge. |

### Acceptance evidence (final addendum)

```
$ ! grep -rniE 'zfs|incus|zpool|incus-ready' docs/design/do-vm-harness.md   -> exit 0, no output
$ grep -n 'mkfs.btrfs' …                       -> 34, 332, 456, 561
$ grep -n 'user_subvol_rm_allowed,noatime' …   -> 347
$ grep -n -- '--identity' …                    -> 10 hits
$ grep -n 'apparmor' …                         -> 14 hits (§2 steps 1/9/10, §3, §9)
$ grep -n 'critical_btrfs_live' …              -> 170, 171, 231, 233
$ ! grep -riE 'option [ab]|spike [ab]' …       -> exit 0, no output
$ make docs-check                               -> zensical --strict: No issues found; honesty OK
$ uv run --no-project tools/guard.py            -> guard: OK (483 file(s) checked)
$ uv run --no-project tools/plan.py validate    -> plan OK
```

### Legality-confirm MINORs (final, same attempt-3 pass)

| # | Item | Disposition |
|---|---|---|
| G1 | (safety) `wipefs -n`-clean `or` branch let a non-zero unrecognized superblock reach `mkfs.btrfs -f` | §2 step 4 rc-`2` branch is now conjunctive — empty `blkid` value **and** status-captured readable `dd` **and** all-zero first MiB **and** `wipefs -n` clean — with the `or` deleted; `mkfs` runs only after all four. §9 cell updated (a `wipefs` signature with an otherwise all-zero first MiB still fails closed). |
| G2 | readability `dd` probe had no explicit status capture | It now uses the same `if dd …; then rc=0; else rc=$?; fi` idiom as `blkid`/`findmnt`, so a read failure names device and rc. |
| G3 | `$0.10/GiB-month` hardcoded yet total presented as computed | §7 labels the volume term and total an **estimate** (documented list price, not a fetched quote); `up` prints the line labelled as an estimate and fails closed only on the doctl droplet price. §9 cost cell updated. |
| G4 | rustup installed via unpinned `curl \| sh` while `crun` is digest-pinned | §2 step 8 now requires a pinned versioned `rustup-init` archive binary verified against a SHA-256 module constant before execution, failing closed naming URL and both digests; the `curl https://sh.rustup.rs \| sh` form is forbidden. §9 provisioning row asserts the URL/digest constants. |
| G5 | absent-identity fail-closed sentence omitted `sync` | §1 now names `status`, `test`, `ssh`, **and `sync`** as never materializing or regenerating the default identity. |

### Acceptance evidence (post-legality-confirm)

```
$ ! grep -rniE 'zfs|incus|zpool|incus-ready' docs/design/do-vm-harness.md   -> exit 0, no output
$ grep -n 'mkfs.btrfs' …                       -> 34, 338, 466, 573
$ grep -n 'user_subvol_rm_allowed,noatime' …   -> 353
$ grep -n -- '--identity' …                    -> 10 hits
$ grep -n 'apparmor' …                         -> 14 hits
$ grep -n 'critical_btrfs_live' …              -> 170, 171, 231, 233
$ ! grep -riE 'option [ab]|spike [ab]' …       -> exit 0, no output
$ make docs-check                               -> zensical --strict: No issues found; honesty OK
$ uv run --no-project tools/guard.py            -> guard: OK (483 file(s) checked)
$ uv run --no-project tools/plan.py validate    -> plan OK
```

### Final functional MINORs (closing the pass)

| # | Item | Disposition |
|---|---|---|
| H1 | §9 general `HOME` cell claimed `TMPDIR` for the `docker info` probe that §1 step 3 showed with `HOME` only | §1 `test` step 3 now states the probe runs **under the suite prefix** with `docker info` as its trailing command (carrying `HOME` and `TMPDIR`); the §9 cell says the same. |
| H2 | §1 step 4 restated the `runuser … env -i HOME/CARGO_HOME/RUSTUP_HOME/PATH` list | Replaced with "under the suite prefix below"; the prefix block is the single home for that envelope. |
| H3 | Automount teardown named `umount` + deletions but no `systemctl`/`udevadm` | §2 step 3 now runs `systemctl disable --now 'mnt-volume_*.mount'` (falling back to `umount` when no unit exists), then deletes the udev rule and runs `udevadm control --reload-rules`, then the unit and fstab lines. §9 teardown row and the `render_automount_teardown()` fake list updated (`udevadm`). |

### Acceptance evidence (closing)

```
$ ! grep -rniE 'zfs|incus|zpool|incus-ready' docs/design/do-vm-harness.md   -> exit 0, no output
$ grep -n 'mkfs.btrfs' …                       -> 34, 340, 468, 575
$ grep -n 'user_subvol_rm_allowed,noatime' …   -> 355
$ grep -n -- '--identity' …                    -> 10 hits
$ grep -n 'apparmor' …                         -> 14 hits
$ grep -n 'critical_btrfs_live' …              -> 170, 171, 231, 233
$ ! grep -riE 'option [ab]|spike [ab]' …       -> exit 0, no output
$ make docs-check                               -> zensical --strict: No issues found; honesty OK
$ uv run --no-project tools/guard.py            -> guard: OK (483 file(s) checked)
$ uv run --no-project tools/plan.py validate    -> plan OK
```

### IoC-confirm findings (final)

| # | Item | Disposition |
|---|---|---|
| J1 | (MAJOR) owner `ssh` envelope reused the suite `PATH`, which omits `target/debug`, so bare `slipfork` (and the `ssh -- slipfork doctor` live cell) could not resolve | §1 `ssh` now exports an owner `PATH` = `target/debug` + suite `PATH`, asserts `command -v slipfork` in the envelope, fails closed naming `tools/do_vm.py test` (which builds the CLI in step 4), and runs `exec bash` with no login profile to reset the exported `HOME`/`TMPDIR`/`PATH`. §9 row updated. |
| J2 | `test` re-implemented host checks with raw `stat -f`/`id` and never proved `user_subvol_rm_allowed` | New §1 `test` step 5 makes `runuser -u slipfork -- target/debug/slipfork doctor --json --state-dir /var/lib/slipfork` (exit 0) the single host-capability gate — it covers super-magic, the `user_subvol_rm_allowed` round-trip, `unshare_userns`, `netns`; raw `stat`/`id` are echoed only as diagnostics when `doctor` fails, and step 1 keeps only the synced-copy and marker checks. §9 row updated; build step 4 now names the doctor gate and renderer as its consumers. |
| J3 | no droplet-free proof of profile body equality | §3 now states CI builds the CLI and compares `render_provision_script()`'s body with `daemon unit --only apparmor`; §9 adds the offline cell. |
| J4 | `status`/`test`/`ssh` accepted repeatable `--identity` but used only the first | §1 table now rejects two or more with a usage error (exit 2) so a listed key is never silently unused; §9 cell updated. (`up` stays repeatable.) Note for `lead`: this tightens `NODE.md`'s "repeatable `--identity`" wording for those three subcommands. |
| J5 | repair path paid the full readiness bound before re-provisioning | §1 `up` step 6 states a reachable droplet satisfies the readiness probe at the first attempt, so a marker-delete repair costs at most the 5-minute SSH gate, not the 15-minute bound; §3 repeats the cost. |

### Acceptance evidence (final)

```
$ ! grep -rniE 'zfs|incus|zpool|incus-ready' docs/design/do-vm-harness.md   -> exit 0, no output
$ grep -n 'mkfs.btrfs' …                       -> 34, 357, 490, 597
$ grep -n 'user_subvol_rm_allowed,noatime' …   -> 372
$ grep -n -- '--identity' …                    -> 10 hits
$ grep -n 'apparmor' …                         -> 16 hits
$ grep -n 'critical_btrfs_live' …              -> 172, 173, 241, 243
$ ! grep -riE 'option [ab]|spike [ab]' …       -> exit 0, no output
$ make docs-check                               -> zensical --strict: No issues found; honesty OK
$ uv run --no-project tools/guard.py            -> guard: OK (483 file(s) checked)
$ uv run --no-project tools/plan.py validate    -> plan OK
```

_Line-number refresh after making the `doctor` gate path absolute (content unchanged):_
`mkfs.btrfs` 34, 359, 492, 599; `user_subvol_rm_allowed,noatime` 374; `critical_btrfs_live`
172, 173, 243, 245; `apparmor` 16 hits; `--identity` 10 hits; both inverted greps exit 0 with
no output; `make docs-check` / `guard.py` / `plan.py validate` all OK.
