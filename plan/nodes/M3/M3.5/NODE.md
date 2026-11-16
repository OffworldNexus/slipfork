---
id: M3.5
title: "DigitalOcean dev-VM harness: droplet + ZFS volume + Incus pool, remote live IncusEngine tests (tools/do_vm.py)"
kind: task
status: done
owner: implementer
priority: 1
depends_on: []
paths: [tools/do_vm.py, "tools/tests/**", "docs/design/do-vm-harness.md", Makefile]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-16T12:31"
updated: "2026-11-16T13:31"
---

# Goal

Ship `tools/do_vm.py`: a **DigitalOcean dev-VM harness** that provisions a throwaway droplet, attaches a block-storage volume, formats it **ZFS**, installs **Incus**, creates an Incus `zfs` storage pool on that volume, syncs the repo, runs the live `IncusEngine` conformance suite remotely, and tears everything down — so `H-004` (no `/var/lib/incus/unix.socket` on this host) stops blocking live hypervisor verification.

Owner's requirement, verbatim: *"use [doctl] to create a VM, attach a ZFS volume, install Incus and run tests in this remote VM. Maybe you create some automation script to automatically setup the VM, run tests then kill the VM (optionally in a second step, because the first time I'll ask you to setup the VM and then let me log in there when it works so that I can try it myself)"* → **`up` must not destroy anything**; teardown is a separate explicit command.

## Commands (exactly these, one canonical way each)

```
tools/do_vm.py up      [--region lon1] [--size s-4vcpu-8gb] [--volume-gb 100] [--yes]
tools/do_vm.py status
tools/do_vm.py sync    # tar-pipe the working tree (git HEAD + uncommitted) to /root/slipfork
tools/do_vm.py test    [-- <extra cargo args>]   # live IncusEngine suite, streamed
tools/do_vm.py ssh
tools/do_vm.py down    [--yes]
```

## Behaviour

- **`up`**: create droplet (Ubuntu 24.04 x64, user-data cloud-init) + volume in the same region, attach the volume, and **wait** until cloud-init reports `slipfork ready` (poll over SSH with a bounded timeout, streaming the last lines on failure). Print IP, SSH command, monthly cost of the chosen size+volume, and the exact `down` command. **Idempotent**: if a droplet/volume tagged `slipfork-dev-vm` already exists, reuse and report instead of creating a second one.
- **Cloud-init provisioning** (single `runcmd` chain, `set -euo pipefail`): install `zfsutils-linux` + `incus`; `zpool create -o ashift=12 -O mountpoint=/var/lib/incus/disks/pool incuspool /dev/disk/by-id/scsi-0DO_Volume_*`; `incus storage create default zfs source=incuspool`; enable `incus` and wait for `incus info` to answer; write `/root/incus-ready`. If the `incus` package is unavailable in the image, **fail with the exact `apt-cache policy incus` output** — never silently switch to a third-party repo.
- **`sync`**: `tar` the working tree (respecting `.gitignore`, excluding `target/`, `site/`, `.worktrees/`) over SSH into `/root/slipfork` on the VM, no rsync dependency. Never sends `.env`-class secrets (the repo has none; assert and skip if present).
- **`test`**: run the live Incus conformance suite on the VM (`cargo test -p slipfork-engine --test critical_incus_live -- --test-threads=1`), stream output live, exit with its status. The suite is `M3.3`'s deliverable — `test` must fail with a clear message while that file does not exist yet.
- **`down`**: destroy the droplet **and** the volume (volume first, after the droplet is gone), by tag; `--yes` guard mirroring `up`.
- **Zero hardcoded account data**: no team name, no SSH-key id, no project id. Resolve the SSH key from `doctl compute ssh-key list` (fail with a clear message when there are zero or several keys, naming `--ssh-key` to disambiguate). Region/size validated against `doctl compute size/region list` before creating anything.
- **Fail closed everywhere**: no bare `except`, no `|| true`, no "retry forever". Every doctl call is checked and its stderr surfaced.
- Python: stdlib only, `uv run --no-project` shebang, `# /// script` block; `subprocess.run(..., check=...)` with argv lists (never `shell=True`).

## Docs

Add `docs/design/do-vm-harness.md` (excluded from the public site) documenting the commands, the cost model, what ZFS buys us (`ioctl(FICLONE)`-class CoW snapshots for `IncusEngine`), and the explicit non-goal (this is a **test** harness, not a supported deployment path). Nothing on the public docs site (no published command changes).

# Acceptance

- [ ] `uv run --no-project tools/do_vm.py --help` exits 0 and lists exactly the six commands.
- [ ] `uv run --no-project tools/do_vm.py up --help` exits 0 and shows `--region/--size/--volume-gb/--yes`.
- [ ] `uv run --no-project tools/do_vm.py test` exits non-zero with a message naming the missing `critical_incus_live` suite (before `M3.3` lands).
- [ ] `uv run --no-project --with pytest pytest -q tools/tests` exits 0 (add unit tests for arg building + tag resolution using a fake `doctl` on `PATH`; never call the real API in tests).
- [ ] `rg -n "dogeops|DogeOps|60390733" tools/do_vm.py` exits 1 (no account-specific literals).
- [ ] `uvx --offline ruff check tools && uvx --offline ruff format --check tools` exits 0.
- [ ] `make check` exits 0.
