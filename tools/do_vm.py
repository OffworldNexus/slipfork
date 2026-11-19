#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""DigitalOcean dev-VM harness: Btrfs volume, docker/Rust, `:80` socket activation (`M3.16`).

Boots one disposable Ubuntu 24.04 droplet, attaches a 100 GiB block-storage volume formatted
**Btrfs** at `/var/lib/slipfork` (`user_subvol_rm_allowed,noatime`), provisions `btrfs-progs`,
`docker.io`/`docker-buildx`, and a Rust toolchain, links the account's `~/.cache` onto that
volume, syncs the working tree, builds the product binaries (named by `cargo metadata`) and
installs them into `/usr/local/bin`, and runs `slipfork daemon install` so `slipforkd.socket`
owns `:80` (the `userns` AppArmor profile and the control-socket handover are rendered by that
product command only; this harness never writes either). `sync` also stamps the tracked
`support/demo` template at `/home/slipfork/demo` once.
No host `crun` is installed: the pinned static binary ships inside the `L0` world rootfs.

`up` is idempotent by the tag `slipfork-dev-vm` and never reformats a device that carries a
filesystem. A volume this invocation creates is formatted Btrfs; a pre-existing blank volume is
only formatted when the operator passes `--format-blank`, and only after the first 8 MiB read
back as all zeros (a filesystem can keep label copies at 0..4 MiB). A detected foreign
filesystem always fails closed, even with `--format-blank`. When the tagged droplet exists but
the harness identity cannot open it, `up` detaches the volume, destroys the droplet, and
recreates it on the same volume. `down --yes` is the only command that deletes the volume; it
keeps it unless `--delete-volume` is given.

  tools/do_vm.py up      [--region ams3] [--size s-4vcpu-8gb] [--volume-gb 100]
                         [--identity PATH ...] [--authorized-key PATH ...]
                         [--format-blank] [--yes]
  tools/do_vm.py status  [--identity PATH ...]
  tools/do_vm.py sync
  tools/do_vm.py test    [--identity PATH ...]
  tools/do_vm.py ssh     [--identity PATH ...] [-- <remote command> ...]
  tools/do_vm.py down    [--yes] [--delete-volume]

`sync` is also the install step: it seeds `support/demo` at `/home/slipfork/demo` once, builds the
product binaries (their names come from `cargo metadata`, in the `debug` profile), installs them
into `/usr/local/bin`, and runs `slipfork daemon install --user slipfork --bin-dir
/usr/local/bin --bin-dir <tree>/target/{debug,release}` — the single writer of the units and the
single owner of the socket handover — before asserting `systemctl is-active slipforkd.socket`.
Both `sync` and `test` therefore stop the installed units and hand the control socket over from
any running `slipforkd`, so every running world's processes stop until the next `slipfork up`. A
missing binary fails closed naming `tools/do_vm.py test`.

Design notes: `docs/design/do-vm-harness.md`. This is a test harness, never a supported
deployment path.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import subprocess
import sys
import tarfile
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, TypeVar

import guard

T = TypeVar("T")

TAG = "slipfork-dev-vm"
IMAGE_SLUG = "ubuntu-24-04-x64"
DEFAULT_REGION = "ams3"
DEFAULT_SIZE = "s-4vcpu-8gb"
DEFAULT_VOLUME_GB = 100
MIN_VOLUME_GB = 10
# DigitalOcean vendor list price as of 2026-11-16 — re-verify before relying on the cost line.
VOLUME_USD_PER_GIB_MONTH = 0.10
READY_TIMEOUT_SECONDS = 15 * 60
READY_POLL_SECONDS = 5
SSH_WAIT_TIMEOUT_SECONDS = 5 * 60
COLD_BOOT_GRACE_SECONDS = 90
DETACH_TIMEOUT_SECONDS = 5 * 60
DROPLET_DELETE_TIMEOUT_SECONDS = 2 * 60
ACCOUNT_KEY_TIMEOUT_SECONDS = 60
DOWN_POLL_TIMEOUT_SECONDS = 60
REMOTE_USER = "slipfork"
REMOTE_HOME = "/home/slipfork"
REMOTE_TREE = f"{REMOTE_HOME}/slipfork"
REMOTE_STAGING = f"{REMOTE_HOME}/.sync"
REMOTE_DEMO = f"{REMOTE_HOME}/demo"
STATE_DIR = "/var/lib/slipfork"
# The unprivileged account's cache tree (`~/.cache`, which holds the default `slipfork` state
# root) is a symlink onto the Btrfs volume, so an ordinary login-shell `slipfork` and the
# installed unit name one state root with no VM-only `--state-dir` on either side.
CACHE_SYMLINK = f"{REMOTE_HOME}/.cache"
CACHE_TARGET = f"{STATE_DIR}/home/.cache"
# The account's single state root, reached through `CACHE_SYMLINK`; the installed unit renders
# exactly this path as `--state-dir`/`--runtime-dir`.
INSTALL_BIN_DIR = "/usr/local/bin"
# The workspace packages whose binary targets are the shipped product.
PRODUCT_BUILD_PACKAGES = ("slipfork", "slipfork-engine")
# The exact binary targets those packages must provide (asserted against `cargo metadata`).
PRODUCT_BINARIES = (
    "slipfork",
    "slipforkd",
    "slipfork-world-init",
    "git-remote-slipfork",
)
BUILD_PROFILE = "debug"
# `cargo metadata --format-version 1 --no-deps` is the one source of the binary target list.
PRODUCT_METADATA = "cargo metadata --format-version 1 --no-deps"
# The AppArmor profile also names these directories, so release builds (benchmarks) are attached.
PROFILE_BIN_PROFILES = ("debug", "release")
DEMO_TEMPLATE = "support/demo"
DEMO_STAMP = "slipfork-demo-stamp"
DEMO_GIT_NAME = "Slipfork Demo"
DEMO_GIT_EMAIL = "demo@slipfork.invalid"
LOGIN_BANNER = "/etc/profile.d/slipfork-demo.sh"
SUITE_HOME = f"{STATE_DIR}/home"
# One short disk-backed test root on the Btrfs volume: `<state>/ctl/<world>/netns.sock` must fit
# the 107-byte `AF_UNIX` limit, so the suites never build it under the deeper suite `HOME`.
SUITE_TEST_ROOT = f"{STATE_DIR}/t"
OWNER_HOME = f"{STATE_DIR}/root-home"
MOUNT_OPTIONS = "user_subvol_rm_allowed,noatime,nofail"
BTRFS_MAGIC = "9123683e"
MARKER = f"{STATE_DIR}/.slipfork-ready"
VOLUME_GLOB = "/dev/disk/by-id/scsi-0DO_Volume_*"
FSTAB = "/etc/fstab"
UDEV_AUTOMOUNT_RULE = "/etc/udev/rules.d/99-digitalocean-automount.rules"
UNIT_DIRS = ("/etc/systemd/system", "/run/systemd/system")
PACKAGES = (
    "btrfs-progs docker.io docker-buildx openssh-server apparmor curl "
    "ca-certificates build-essential git"
)
RUSTUP_VERSION = "1.29.3"
RUSTUP_INIT_URL = (
    "https://static.rust-lang.org/rustup/archive/"
    f"{RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init"
)
RUSTUP_INIT_SHA256 = "b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55"
RUST_MIN_VERSION = "1.94.0"
# Every entry is `(cargo package, integration test binary, repo-relative file, owning node)`.
# The list runs one at a time (`--test-threads=1`): each suite boots real worlds through the
# production `slipforkd`, so the VM exercises the Btrfs-only positive halves of the CLI suites too.
SUITES = (
    (
        "slipfork-engine",
        "critical_btrfs_live",
        "crates/slipfork-engine/tests/critical_btrfs_live.rs",
        "M3.14",
    ),
    (
        "slipfork-engine",
        "critical_engine",
        "crates/slipfork-engine/tests/critical_engine.rs",
        "M3.14",
    ),
    (
        "slipfork-engine",
        "critical_oci_runtime",
        "crates/slipfork-engine/tests/critical_oci_runtime.rs",
        "M3.14",
    ),
    (
        "slipfork-engine",
        "critical_pod_runtime",
        "crates/slipfork-engine/tests/critical_pod_runtime.rs",
        "M3.32.2.2.1.3.2.1",
    ),
    (
        "slipfork-edge",
        "critical_edge_live",
        "crates/slipfork-edge/tests/critical_edge_live.rs",
        "M3.30.4.4",
    ),
    (
        "slipfork",
        "critical_btrfs_live",
        "crates/slipfork/tests/critical_btrfs_live.rs",
        "M3.15",
    ),
    (
        "slipfork",
        "critical_cli",
        "crates/slipfork/tests/critical_cli.rs",
        "M3.15",
    ),
    (
        "slipfork",
        "critical_l0_wiring",
        "crates/slipfork/tests/critical_l0_wiring.rs",
        "M2.29",
    ),
    (
        "slipfork",
        "critical_slug",
        "crates/slipfork/tests/critical_slug.rs",
        "M3.15",
    ),
    (
        "slipfork",
        "critical_ospp_daemon",
        "crates/slipfork/tests/critical_ospp_daemon.rs",
        "M3.15",
    ),
    (
        "slipfork",
        "critical_world_journey",
        "crates/slipfork/tests/critical_world_journey.rs",
        "M3.30.4",
    ),
    (
        "slipfork",
        "critical_vcs_remote",
        "crates/slipfork/tests/critical_vcs_remote.rs",
        "M3.30.4",
    ),
)
AUTHORIZED_KEY_USERS = ("root", REMOTE_USER)
EXCLUDED_TOP_LEVEL = frozenset({"target", "site", ".worktrees"})
SSH_AUTH_FAILURE = (
    "Permission denied (publickey",
    "Too many authentication failures",
)
SSH_HOST_KEY_FAILURE = (
    "REMOTE HOST IDENTIFICATION HAS CHANGED",
    "Host key verification failed",
)


def authorized_keys_paths(remote_home: str = REMOTE_HOME) -> tuple[str, ...]:
    """The one canonical root + suite-user `authorized_keys` enumeration."""
    return ("/root/.ssh/authorized_keys", f"{remote_home}/.ssh/authorized_keys")


def _authorized_key_entries(
    remote_home: str = REMOTE_HOME,
) -> tuple[tuple[str, str], ...]:
    """Pair each `authorized_keys` path with its owning account (no account lookup needed)."""
    return tuple(
        zip(AUTHORIZED_KEY_USERS, authorized_keys_paths(remote_home), strict=True)
    )


def _q(text: str) -> str:
    """Quote one config path/word for a remote shell (`shlex.quote` is a no-op when it is safe)."""
    return shlex.quote(text)


class HarnessError(RuntimeError):
    """A fail-closed harness precondition, API call, or remote step failed."""


class ProvisioningError(HarnessError):
    """The remote provisioning script refused; carry its exit status to the caller."""

    def __init__(self, message: str, returncode: int) -> None:
        super().__init__(message)
        self.returncode = returncode


@dataclass(frozen=True, slots=True)
class Droplet:
    id: int
    name: str
    region: str
    size: str
    ip: str
    volume_ids: frozenset[str]


@dataclass(frozen=True, slots=True)
class Volume:
    id: str
    name: str
    region: str
    size_gb: int
    droplet_ids: frozenset[int]


@dataclass(frozen=True, slots=True)
class Size:
    slug: str
    vcpus: int | None
    memory_mib: int | None
    disk_gib: int | None
    price_monthly: float | None
    regions: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class Identity:
    path: Path
    public_key: str
    public_key_file: Path


@dataclass(frozen=True, slots=True)
class ProvisionConfig:
    """Everything the provisioning script interpolates, so renderers never read globals."""

    volume_glob: str = VOLUME_GLOB
    mountpoint: str = STATE_DIR
    fstab: str = FSTAB
    udev_rule: str = UDEV_AUTOMOUNT_RULE
    unit_dirs: tuple[str, ...] = UNIT_DIRS
    marker: str = MARKER
    suite_home: str = SUITE_HOME
    test_root: str = SUITE_TEST_ROOT
    owner_home: str = OWNER_HOME
    remote_home: str = REMOTE_HOME
    remote_tree: str = REMOTE_TREE
    remote_demo: str = REMOTE_DEMO
    remote_user: str = REMOTE_USER
    cache_symlink: str = CACHE_SYMLINK
    cache_target: str = CACHE_TARGET
    login_banner: str = LOGIN_BANNER
    install_bin_dir: str = INSTALL_BIN_DIR
    binaries: tuple[str, ...] = PRODUCT_BINARIES
    build_profile: str = BUILD_PROFILE
    bin_profiles: tuple[str, ...] = PROFILE_BIN_PROFILES
    demo_template: str = DEMO_TEMPLATE
    demo_stamp: str = DEMO_STAMP
    demo_git_name: str = DEMO_GIT_NAME
    demo_git_email: str = DEMO_GIT_EMAIL
    mount_options: str = MOUNT_OPTIONS
    btrfs_magic: str = BTRFS_MAGIC
    packages: str = PACKAGES
    rustup_url: str = RUSTUP_INIT_URL
    rustup_sha256: str = RUSTUP_INIT_SHA256
    rust_min: str = RUST_MIN_VERSION
    apt_wait_attempts: int = 30
    apt_wait_seconds: int = 5
    device_wait_attempts: int = 30
    device_wait_seconds: int = 2
    allow_format: bool = False


PROVISION = ProvisionConfig()


def render_volume_discovery(config: ProvisionConfig = PROVISION) -> str:
    """Select the one attached volume *disk*, ignoring its `-partN` partition symlinks."""
    return f"""\
volume_device=""
for _attempt in $(seq 1 {config.device_wait_attempts}); do
  _candidates=""
  for _candidate in {config.volume_glob}; do
    case "$_candidate" in
      *-part[0-9]*) continue ;;
    esac
    if [ -e "$_candidate" ]; then
      _candidates="$_candidates $_candidate"
    fi
  done
  set -- $_candidates
  if [ "$#" -eq 1 ]; then
    volume_device="$1"
    break
  fi
  sleep {config.device_wait_seconds}
done
if [ -z "$volume_device" ]; then
  _found=""
  for _candidate in {config.volume_glob}; do
    if [ -e "$_candidate" ]; then
      _found="$_found $_candidate"
    fi
  done
  echo "slipfork: expected exactly one attached DO volume disk ({config.volume_glob}, partitions excluded), found:${{_found:- none}}" >&2
  exit 1
fi
"""


def render_automount_teardown(config: ProvisionConfig = PROVISION) -> str:
    """Stop and delete DigitalOcean's optional "Automatically Format & Mount" wiring."""
    unit_removals = "\n".join(
        f"rm -f {directory}/mnt-volume_*.mount" for directory in config.unit_dirs
    )
    return f"""\
_units_err=$(mktemp)
if _units_raw=$(systemctl list-units --all --plain --no-legend 'mnt-volume_*.mount' 2>"$_units_err"); then
  _units=$(printf '%s\n' "$_units_raw" | awk '{{print $1}}')
  for _unit in $_units; do
    case "$_unit" in
      *.mount) systemctl disable --now "$_unit" ;;
    esac
  done
else
  _units_rc=$?
  echo "slipfork: systemctl list-units failed (rc $_units_rc); cannot prove no DigitalOcean automount unit exists: $(cat "$_units_err")" >&2
  rm -f "$_units_err"
  exit 1
fi
rm -f "$_units_err"
if _targets=$(findmnt -rn -S "$volume_device" -o TARGET); then
  for _target in $_targets; do
    case "$_target" in
      /mnt/*) umount "$_target" ;;
    esac
  done
fi
rm -f {config.udev_rule}
udevadm control --reload-rules
{unit_removals}
systemctl daemon-reload
if [ -f {config.fstab} ]; then
  _volume_name=$(basename "$volume_device")
  awk -v dev="$volume_device" -v name="$_volume_name" '
    $2 ~ /^\\/mnt\\// && ($1 == dev || index($1, name) > 0) {{ next }}
    {{ print }}
  ' {config.fstab} > {config.fstab}.slipfork
  mv {config.fstab}.slipfork {config.fstab}
fi
"""


def render_volume_signature_step(config: ProvisionConfig = PROVISION) -> str:
    """Branch on `blkid`'s exit status; only a proven-empty device is formatted.

    A blank device is formatted only when this invocation created the volume or the operator
    passed `--format-blank`. The emptiness proof reads 8 MiB, because a filesystem can keep label copies at
    0..4 MiB and a 1 MiB probe was proven to miss one. A detected foreign filesystem always
    fails closed, flag or not.
    """
    format_allowed = 1 if config.allow_format else 0
    return f"""\
_format_allowed={format_allowed}
_blkid_err=$(mktemp)
if _type=$(blkid -p -s TYPE -o value "$volume_device" 2>"$_blkid_err"); then
  _type_rc=0
else
  _type_rc=$?
fi
_type_err=$(cat "$_blkid_err")
rm -f "$_blkid_err"
case "$_type_rc" in
  0)
    if [ -z "$_type" ]; then
      echo "slipfork: blkid reported success but no filesystem type for $volume_device; wipefs -n says:" >&2
      wipefs -n "$volume_device" >&2
      exit 1
    fi
    if [ "$_type" != "btrfs" ]; then
      echo "slipfork: $volume_device carries filesystem type '$_type', not btrfs; refusing to format (a detected foreign filesystem needs an operator wipe)" >&2
      exit 1
    fi
    ;;
  2)
    if [ -n "$_type" ]; then
      echo "slipfork: blkid exited 2 but reported type '$_type' for $volume_device; refusing to format" >&2
      exit 1
    fi
    if [ "$_format_allowed" -ne 1 ]; then
      echo "slipfork: blank volume detected; re-run with --format-blank after confirming it holds no data" >&2
      exit 1
    fi
    if _wipefs=$(wipefs -n "$volume_device"); then
      _wipefs_rc=0
    else
      _wipefs_rc=$?
    fi
    if [ "$_wipefs_rc" -ne 0 ]; then
      echo "slipfork: wipefs -n $volume_device failed (rc $_wipefs_rc); refusing to format" >&2
      exit 1
    fi
    if [ -n "$(printf '%s' "$_wipefs" | tr -d '[:space:]')" ]; then
      echo "slipfork: wipefs -n reports signatures on $volume_device; refusing to format" >&2
      exit 1
    fi
    # A filesystem can keep label copies at 0..4 MiB; this session proved a 1 MiB probe misses one, so
    # the emptiness proof reads the first 8 MiB and requires every byte to be zero.
    _dd_err=$(mktemp)
    if dd if="$volume_device" of=/dev/null bs=1M count=8 2>"$_dd_err"; then
      _dd_rc=0
    else
      _dd_rc=$?
    fi
    _dd_err_text=$(cat "$_dd_err")
    rm -f "$_dd_err"
    if [ "$_dd_rc" -ne 0 ]; then
      echo "slipfork: reading the first 8 MiB of $volume_device failed (rc $_dd_rc): ${{_dd_err_text:-no stderr}}; refusing to format" >&2
      exit 1
    fi
    if ! cmp -s <(head -c 8388608 "$volume_device") <(head -c 8388608 /dev/zero); then
      echo "slipfork: $volume_device has non-zero data in its first 8 MiB; refusing to format" >&2
      exit 1
    fi
    echo "slipfork: blank volume evidence for $volume_device: blkid TYPE=<none> (rc 2); wipefs -n=<no signatures>; first 8 MiB=all zero"
    mkfs.btrfs -f -L slipfork "$volume_device"
    ;;
  *)
    echo "slipfork: blkid failed for $volume_device (rc $_type_rc): ${{_type_err:-no stderr}}" >&2
    exit 1
    ;;
esac
if _uuid=$(blkid -p -s UUID -o value "$volume_device"); then
  _uuid_rc=0
else
  _uuid_rc=$?
fi
if [ "$_uuid_rc" -ne 0 ] || [ -z "$_uuid" ]; then
  echo "slipfork: could not read the filesystem UUID of $volume_device (rc $_uuid_rc)" >&2
  exit 1
fi
"""


def render_install_authorized_keys(config: ProvisionConfig = PROVISION) -> str:
    """Install the key blob on stdin for root and the suite user (one canonical enumeration).

    Each `user path` pair carries both facts the step needs, so no account-database lookup
    (`getent passwd`/`id`) runs to rediscover a home directory this harness already names. The
    pair is passed as two positional words, so a path that needs quoting survives the shell.
    """
    pairs = " ".join(
        f"{_q(user)} {_q(path)}"
        for user, path in _authorized_key_entries(config.remote_home)
    )
    return f"""\
set -euo pipefail
_key_blob=$(mktemp)
cat > "$_key_blob"
set -- {pairs}
while [ "$#" -gt 0 ]; do
  _user=$1
  _path=$2
  shift 2
  _home=${{_path%/.ssh/authorized_keys}}
  install -d -m 700 -o "$_user" -g "$_user" "$_home/.ssh"
  touch "$_path"
  chmod 600 "$_path"
  chown "$_user:$_user" "$_path"
  cat "$_key_blob" >> "$_path"
  sort -u -o "$_path" "$_path"
done
rm -f "$_key_blob"
"""


def render_login_banner(config: ProvisionConfig = PROVISION) -> str:
    """The one login banner provisioning writes: the account plus the demo's shipped verbs.

    The banner is a guard, not an advert: it prints only when the seeded demo directory exists
    and a `slipfork` is on `PATH`, so a host brought up by provisioning alone (before `sync`
    seeds the demo and installs the CLI) never claims something that is not there.
    """
    return f"""\
# Slipfork dev-VM quickstart banner, written by `tools/do_vm.py` provisioning (harness-owned).
if [ -n "${{BASH_VERSION:-}}" ] && [ -t 1 ]; then
  _slipfork_account=$(id -un)
  if {{ [ "$_slipfork_account" = "{config.remote_user}" ] ||
       [ "$_slipfork_account" = "root" ]; }} &&
     [ -d {_q(config.remote_demo)} ] && command -v slipfork >/dev/null 2>&1; then
    cat <<'SLIPFORK'

Slipfork demo — account: {config.remote_user} (from root: sudo -iu {config.remote_user})
  cd ~/demo
  slipfork
  slipfork up
  slipfork ps
SLIPFORK
  fi
  unset _slipfork_account
fi
"""


def render_product_install(config: ProvisionConfig = PROVISION) -> str:
    """Install the four binaries, release a daemon from an older build, then hand `:80` over.

    `slipfork daemon install` is the only writer of `slipforkd.{socket,service}` and of
    `/etc/apparmor.d/slipfork`; this script never renders a unit or a profile body, it installs
    the executables the AppArmor attachment names, and then asserts the product's own verbs
    took effect. `slipfork daemon install` owns the socket handover (any holder, including a
    daemon from an older build), so this step adds no daemon lifecycle of its own. Every branch
    fails closed: a missing binary, a non-`active` socket unit, an unloaded profile, or a
    denied unprivileged `CLONE_NEWUSER` exits non-zero.
    """
    sources = f'  _source={_q(f"{config.remote_tree}/target/{config.build_profile}")}/"$_binary"'
    installs = "\n".join(
        [
            '  if [ ! -x "$_source" ]; then',
            "    echo \"slipfork: $_source is missing or not executable; run 'tools/do_vm.py test' to build the product binaries\" >&2",
            "    exit 1",
            "  fi",
            f'  if ! cmp -s "$_source" {_q(config.install_bin_dir)}/"$_binary"; then',
            f'    install -o root -g root -m 0755 "$_source" {_q(config.install_bin_dir)}/"$_binary"',
            "  fi",
        ]
    )
    bin_dirs = " ".join(
        f"--bin-dir {_q(directory)}"
        for directory in (
            config.install_bin_dir,
            *(
                f"{config.remote_tree}/target/{profile}"
                for profile in config.bin_profiles
            ),
        )
    )
    daemon_install = (
        f"{_q(config.install_bin_dir)}/slipfork daemon install "
        f"--user {_q(config.remote_user)} {bin_dirs}"
    )
    return f"""\
set -euo pipefail
for _binary in {" ".join(config.binaries)}; do
{sources}
{installs}
done
{daemon_install}
if _socket_state=$(systemctl is-active slipforkd.socket); then
  _socket_rc=0
else
  _socket_rc=$?
fi
if [ "$_socket_rc" -ne 0 ] || [ "$_socket_state" != "active" ]; then
  echo "slipfork: systemctl is-active slipforkd.socket reported '${{_socket_state:-no output}}' (rc $_socket_rc), not active" >&2
  exit 1
fi
if _aa_status=$(aa-status); then
  _aa_rc=0
else
  _aa_rc=$?
fi
if [ "$_aa_rc" -ne 0 ]; then
  echo "slipfork: aa-status failed (rc $_aa_rc): $_aa_status" >&2
  exit 1
fi
case "$_aa_status" in
  *slipfork*) ;;
  *)
    echo "slipfork: the slipfork userns profile is not loaded (aa-status has no slipfork profile):" >&2
    printf '%s\\n' "$_aa_status" >&2
    exit 1
    ;;
esac
if ! runuser -u {_q(config.remote_user)} -- unshare --user \\
  --map-user="$(id -u {_q(config.remote_user)})" \\
  --map-group="$(id -g {_q(config.remote_user)})" -- true; then
  echo "slipfork: unprivileged userns creation is still denied for {config.remote_user}" >&2
  exit 1
fi
"""


def render_demo_seed(config: ProvisionConfig = PROVISION) -> str:
    """Seed the tracked demo template once, as one Git commit, and never clobber later edits.

    A seeded demo carries the stamp file inside its own `.git`, so every later `sync` reports it
    unchanged and touches nothing. A directory that exists without the stamp is somebody's work:
    it fails closed and names the path instead of copying over it.
    """
    git = (
        f"runuser -u {_q(config.remote_user)} -- env HOME={_q(config.remote_home)} "
        "GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 "
        f'git -C "$_demo"'
    )
    template = _q(f"{config.remote_tree}/{config.demo_template}")
    return f"""\
set -euo pipefail
_demo={_q(config.remote_demo)}
_stamp="$_demo/.git/{config.demo_stamp}"
if [ -f "$_stamp" ]; then
  echo "demo     unchanged $_demo (seeded once; owner edits are kept)"
  exit 0
fi
if [ -e "$_demo" ]; then
  echo "slipfork: $_demo exists but carries no $_stamp; move it aside and re-run 'tools/do_vm.py sync'" >&2
  exit 1
fi
if [ ! -d {template} ]; then
  echo "slipfork: {template} is missing; run 'tools/do_vm.py sync' from the repository root" >&2
  exit 1
fi
install -d -o {_q(config.remote_user)} -g {_q(config.remote_user)} -m 755 "$_demo"
cp -a {template}/. "$_demo"/
chown -R {_q(config.remote_user)}:{_q(config.remote_user)} "$_demo"
{git} init --quiet --initial-branch=main
{git} config user.name {_q(config.demo_git_name)}
{git} config user.email {_q(config.demo_git_email)}
{git} add --all
{git} commit --quiet --message 'Slipfork 0.1.0 demo (seeded by tools/do_vm.py sync)'
printf 'seeded by tools/do_vm.py sync\\n' > "$_stamp"
chown {_q(config.remote_user)}:{_q(config.remote_user)} "$_stamp"
echo "demo     seeded $_demo (one commit; owner edits are kept from now on)"
"""


def render_provision_script(config: ProvisionConfig = PROVISION) -> str:
    """One idempotent provisioning script, embedded verbatim in cloud-init and re-run over SSH."""
    banner_body = render_login_banner(config)
    return f"""\
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

_apt_ok=0
for _attempt in $(seq 1 {config.apt_wait_attempts}); do
  if apt-get update; then
    _apt_ok=1
    break
  fi
  sleep {config.apt_wait_seconds}
done
if [ "$_apt_ok" -ne 1 ]; then
  echo 'slipfork: apt-get update failed after {config.apt_wait_attempts} attempts' >&2
  exit 1
fi
_apt_ok=0
for _attempt in $(seq 1 {config.apt_wait_attempts}); do
  if apt-get install -y --no-install-recommends {config.packages}; then
    _apt_ok=1
    break
  fi
  sleep {config.apt_wait_seconds}
done
if [ "$_apt_ok" -ne 1 ]; then
  echo 'slipfork: package installation failed; apt-cache policy:' >&2
  apt-cache policy {config.packages} >&2
  exit 1
fi

if ! id -u {_q(config.remote_user)} >/dev/null 2>&1; then
  useradd --create-home --shell /bin/bash {_q(config.remote_user)}
fi
usermod -aG docker {_q(config.remote_user)}

{render_volume_discovery(config)}
{render_automount_teardown(config)}
{render_volume_signature_step(config)}
mkdir -p {_q(config.mountpoint)}
if [ -f {_q(config.fstab)} ]; then
  awk '$2 != "{config.mountpoint}"' {config.fstab} > {config.fstab}.slipfork
  mv {_q(config.fstab)}.slipfork {_q(config.fstab)}
fi
printf 'UUID=%s {_q(config.mountpoint)} btrfs %s 0 0\\n' "$_uuid" "{config.mount_options}" >> {_q(config.fstab)}
if _mount_options=$(findmnt -no OPTIONS {_q(config.mountpoint)}); then
  if ! printf ',%s,' "$_mount_options" | grep -q ',user_subvol_rm_allowed,' ||
     ! printf ',%s,' "$_mount_options" | grep -q ',noatime,'; then
    mount -o remount,user_subvol_rm_allowed,noatime {_q(config.mountpoint)}
  fi
else
  mount {_q(config.mountpoint)}
fi

_verify_options=$(findmnt -no OPTIONS {_q(config.mountpoint)})
case ",$_verify_options," in
  *,user_subvol_rm_allowed,*) ;;
  *)
    echo "slipfork: {_q(config.mountpoint)} is mounted without user_subvol_rm_allowed: $_verify_options" >&2
    exit 1
    ;;
esac
case ",$_verify_options," in
  *,noatime,*) ;;
  *)
    echo "slipfork: {_q(config.mountpoint)} is mounted without noatime: $_verify_options" >&2
    exit 1
    ;;
esac
_magic=$(stat -f -c %t {_q(config.mountpoint)})
if [ "$_magic" != "{config.btrfs_magic}" ]; then
  echo "slipfork: statfs({_q(config.mountpoint)}) f_type is 0x$_magic, not 0x{config.btrfs_magic} (btrfs)" >&2
  exit 1
fi
rm -f {_q(config.marker)}

install -d -o {_q(config.remote_user)} -g {_q(config.remote_user)} {_q(config.suite_home)}
install -d -o {_q(config.remote_user)} -g {_q(config.remote_user)} -m 700 {_q(config.suite_home)}/tmp
install -d -o {_q(config.remote_user)} -g {_q(config.remote_user)} -m 700 {_q(config.test_root)}
install -d -o root -g root -m 700 {_q(config.owner_home)}
install -d -o root -g root -m 700 {_q(config.owner_home)}/tmp
chown {_q(config.remote_user)}:{_q(config.remote_user)} {_q(config.mountpoint)}

# The account's cache tree (`~/.cache`) lives on the Btrfs volume, so an ordinary login-shell
# `slipfork` resolves the same `<state_dir>/run/daemon.sock` the installed unit serves, with no
# VM-only `--state-dir` flag on either side.
install -d -o {_q(config.remote_user)} -g {_q(config.remote_user)} -m 700 {_q(config.cache_target)}
if [ -L {_q(config.cache_symlink)} ]; then
  _cache_target=$(readlink {_q(config.cache_symlink)})
  if [ "$_cache_target" != "{_q(config.cache_target)}" ]; then
    echo "slipfork: {_q(config.cache_symlink)} points at $_cache_target, not {_q(config.cache_target)}" >&2
    exit 1
  fi
elif [ -e {_q(config.cache_symlink)} ]; then
  echo "slipfork: {_q(config.cache_symlink)} is a real directory, not the {_q(config.cache_target)} symlink; move it aside and re-run provisioning" >&2
  exit 1
else
  ln -s {_q(config.cache_target)} {_q(config.cache_symlink)}
  chown -h {_q(config.remote_user)}:{_q(config.remote_user)} {_q(config.cache_symlink)}
fi
_cache_magic=$(stat -f -c %t {_q(config.cache_symlink)})
if [ "$_cache_magic" != "{config.btrfs_magic}" ]; then
  echo "slipfork: {_q(config.cache_symlink)} resolves to a filesystem with f_type 0x$_cache_magic, not 0x{config.btrfs_magic} (btrfs)" >&2
  exit 1
fi

if [ "$(uname -m)" != "x86_64" ]; then
  echo "slipfork: the pinned rustup-init asset targets x86_64, not $(uname -m)" >&2
  exit 1
fi
_rustup_dir=$(mktemp -d)
chmod 755 "$_rustup_dir"
_rustup_init="$_rustup_dir/rustup-init"
curl -fsSL --retry 3 -o "$_rustup_init" "{config.rustup_url}"
_rustup_sha=$(sha256sum "$_rustup_init" | awk '{{print $1}}')
if [ "$_rustup_sha" != "{config.rustup_sha256}" ]; then
  echo "slipfork: rustup-init digest mismatch for {config.rustup_url}: expected {config.rustup_sha256}, got $_rustup_sha" >&2
  rm -rf "$_rustup_dir"
  exit 1
fi
chmod 755 "$_rustup_init"
runuser -u {_q(config.remote_user)} -- env HOME={_q(config.remote_home)} \\
  CARGO_HOME={_q(config.remote_home)}/.cargo \\
  RUSTUP_HOME={_q(config.remote_home)}/.rustup \\
  "$_rustup_init" -y --profile minimal --default-toolchain {config.rust_min} --no-modify-path
rm -rf "$_rustup_dir"
_cargo_version=$(runuser -u {_q(config.remote_user)} -- {_q(config.remote_home)}/.cargo/bin/cargo --version)
_cargo_number=$(printf '%s' "$_cargo_version" | awk '{{print $2}}')
if [ "$(printf '%s\\n%s\\n' "{config.rust_min}" "$_cargo_number" | sort -V | head -n 1)" != "{config.rust_min}" ]; then
  echo "slipfork: cargo $_cargo_number is older than {config.rust_min}: $_cargo_version" >&2
  exit 1
fi

systemctl enable --now docker
docker buildx version
apparmor_parser --version

# One harness-owned login banner. The AppArmor `userns` profile is not rendered here: it is a
# product artifact written by `slipfork daemon install` (see `render_product_install`).
_banner_tmp=$(mktemp)
cat > "$_banner_tmp" <<'SLIPFORK_BANNER'
{banner_body.rstrip()}
SLIPFORK_BANNER
if [ ! -f {_q(config.login_banner)} ] || ! cmp -s {_q(config.login_banner)} "$_banner_tmp"; then
  install -o root -g root -m 644 "$_banner_tmp" {_q(config.login_banner)}
fi
rm -f "$_banner_tmp"

printf 'ready\\n' > {_q(config.marker)}
chown {_q(config.remote_user)}:{_q(config.remote_user)} {_q(config.marker)}
"""


def cloud_init(config: ProvisionConfig = PROVISION) -> str:
    """Wrap the idempotent provisioning script in a single cloud-init `runcmd` chain."""
    body = "\n".join(
        f"      {line}" for line in render_provision_script(config).splitlines()
    )
    return f"#cloud-config\nruncmd:\n  - - bash\n    - -lc\n    - |-\n{body}\n"


def _detail(probe: subprocess.CompletedProcess[str]) -> str:
    return (probe.stderr or probe.stdout or "").strip()


def _short(argv: list[str]) -> str:
    head = " ".join(argv[:5])
    return head if len(argv) <= 5 else f"{head} ..."


def _run(
    argv: list[str],
    *,
    cwd: Path | None = None,
    capture: bool = True,
    check: bool = True,
    input_text: str | None = None,
) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            argv,
            cwd=cwd,
            input=input_text,
            capture_output=capture,
            text=True,
            check=False,
        )
    except OSError as error:
        raise HarnessError(f"could not execute `{argv[0]}`: {error}") from error
    if check and result.returncode != 0:
        raise HarnessError(
            f"command failed ({result.returncode}): {_short(argv)}: {_detail(result)}"
        )
    return result


def _run_stream(argv: list[str]) -> subprocess.Popen[bytes]:
    """The single streaming-process seam (stdin/stdout pipes owned by the caller)."""
    try:
        return subprocess.Popen(argv, stdin=subprocess.PIPE, stderr=subprocess.PIPE)
    except OSError as error:
        raise HarnessError(f"could not execute `{argv[0]}`: {error}") from error


def _json_list(value: Any, what: str) -> list[Any]:
    if not isinstance(value, list):
        raise HarnessError(f"doctl {what} did not return a JSON list")
    return value


def _slug(value: Any) -> str:
    if not isinstance(value, dict) or not isinstance(value.get("slug"), str):
        raise HarnessError(f"doctl returned a malformed region/size object: {value!r}")
    return value["slug"]


class Doctl:
    """Thin checked wrapper around the authenticated local `doctl` CLI."""

    def json(self, *args: str) -> Any:
        result = _run(["doctl", *args, "--output", "json"])
        try:
            return json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise HarnessError(
                f"doctl returned invalid JSON for `{' '.join(args)}`: {error}"
            ) from error

    def tagged_droplets(self) -> list[Droplet]:
        raw = _json_list(
            self.json("compute", "droplet", "list", "--tag-name", TAG), "droplet list"
        )
        droplets: list[Droplet] = []
        for item in raw:
            if not isinstance(item, dict) or not isinstance(item.get("id"), int):
                raise HarnessError(f"doctl droplet list entry is malformed: {item!r}")
            ip = _public_ipv4(item)
            if ip is None:
                raise HarnessError(
                    f"tagged droplet {item.get('id')} has no public IPv4 address"
                )
            droplets.append(
                Droplet(
                    id=item["id"],
                    name=str(item.get("name", TAG)),
                    region=_slug(item.get("region")),
                    size=_slug(item.get("size")),
                    ip=ip,
                    volume_ids=_volume_ids(item),
                )
            )
        return droplets

    def tagged_volumes(self) -> list[Volume]:
        raw = _json_list(self.json("compute", "volume", "list"), "volume list")
        volumes: list[Volume] = []
        for item in raw:
            if not isinstance(item, dict):
                raise HarnessError(f"doctl volume list entry is malformed: {item!r}")
            tags = item.get("tags", [])
            if not isinstance(tags, list) or not all(
                isinstance(tag, str) for tag in tags
            ):
                raise HarnessError(
                    f"doctl volume {item.get('id')!r} returned malformed tags: {tags!r}"
                )
            if TAG not in tags:
                continue
            volume_id = item.get("id")
            if not isinstance(volume_id, str):
                raise HarnessError(
                    f"tagged volume has no string id: {item.get('id')!r}"
                )
            volumes.append(
                Volume(
                    id=volume_id,
                    name=str(item.get("name", TAG)),
                    region=_slug(item.get("region")),
                    size_gb=int(item.get("size_gigabytes", 0)),
                    droplet_ids=_int_set(item.get("droplet_ids", []), "droplet_ids"),
                )
            )
        return volumes

    def regions(self) -> list[str]:
        raw = _json_list(self.json("compute", "region", "list"), "region list")
        regions: list[str] = []
        for item in raw:
            if not isinstance(item, dict):
                raise HarnessError(f"doctl region list entry is malformed: {item!r}")
            if item.get("available", True) is True:
                regions.append(_slug(item))
        return sorted(regions)

    def size(self, slug: str) -> Size | None:
        raw = _json_list(self.json("compute", "size", "list"), "size list")
        for item in raw:
            if not isinstance(item, dict):
                raise HarnessError(f"doctl size list entry is malformed: {item!r}")
            if item.get("slug") == slug:
                price = item.get("price_monthly")
                return Size(
                    slug=slug,
                    vcpus=_optional_int(item.get("vcpus")),
                    memory_mib=_optional_int(item.get("memory")),
                    disk_gib=_optional_int(item.get("disk")),
                    price_monthly=float(price)
                    if isinstance(price, (int, float))
                    else None,
                    regions=_str_tuple(item.get("regions", []), "regions"),
                )
        return None

    def ssh_keys(self) -> list[dict[str, Any]]:
        raw = _json_list(self.json("compute", "ssh-key", "list"), "ssh-key list")
        keys: list[dict[str, Any]] = []
        for item in raw:
            if not isinstance(item, dict) or item.get("id") is None:
                raise HarnessError(f"doctl ssh-key list entry is malformed: {item!r}")
            keys.append(item)
        return keys


def _optional_int(value: Any) -> int | None:
    return int(value) if isinstance(value, int) else None


def _str_set(value: Any, what: str) -> frozenset[str]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise HarnessError(f"doctl returned malformed {what}: {value!r}")
    return frozenset(value)


def _int_set(value: Any, what: str) -> frozenset[int]:
    if not isinstance(value, list) or not all(
        isinstance(item, int) and not isinstance(item, bool) for item in value
    ):
        raise HarnessError(f"doctl returned malformed {what}: {value!r}")
    return frozenset(value)


def _str_tuple(value: Any, what: str) -> tuple[str, ...]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise HarnessError(f"doctl returned malformed {what}: {value!r}")
    return tuple(value)


def _public_ipv4(item: dict[str, Any]) -> str | None:
    networks = item.get("networks")
    if not isinstance(networks, dict):
        return None
    addresses = networks.get("v4", [])
    if not isinstance(addresses, list):
        raise HarnessError(
            f"doctl droplet {item.get('id')} returned malformed networks.v4: {addresses!r}"
        )
    for address in addresses:
        if not isinstance(address, dict):
            raise HarnessError(
                f"doctl droplet {item.get('id')} returned malformed networks.v4 entry: {address!r}"
            )
        if address.get("type") == "public":
            ip = address.get("ip_address")
            if not isinstance(ip, str) or not ip:
                raise HarnessError(
                    f"doctl droplet {item.get('id')} returned a public IPv4 entry without an address"
                )
            return ip
    return None


def _volume_ids(item: dict[str, Any]) -> frozenset[str]:
    return _str_set(item.get("volume_ids", []), "volume_ids")


def repo_root() -> Path:
    try:
        return guard.repository_root(Path.cwd())
    except guard.GuardError as error:
        raise HarnessError(str(error)) from error


def known_hosts_for(root: Path, droplet: Droplet) -> Path:
    directory = root / ".slipfork" / "do-vm"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    return directory / f"known_hosts-{droplet.id}"


def ssh_argv(
    droplet: Droplet,
    known_hosts: Path,
    identity: Identity,
    remote: str | None,
    *,
    batch: bool = True,
    tty: bool = False,
) -> list[str]:
    # `remote` is a shell string sent as one SSH argv operand. Every interpolated operand is a
    # module constant, a `shlex.quote`-escaped extra, or a fixed path; no ambient state leaks in.
    argv = [
        "ssh",
        "-F",
        "/dev/null",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "IdentityAgent=none",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        f"UserKnownHostsFile={known_hosts}",
        "-o",
        "ConnectTimeout=10",
        "-i",
        str(identity.path),
    ]
    if batch:
        argv.extend(
            [
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=30",
                "-o",
                "ServerAliveCountMax=4",
            ]
        )
    if tty:
        argv.append("-t")
    argv.append(f"root@{droplet.ip}")
    if remote is not None:
        argv.append(remote)
    return argv


def default_identity_path(home: Path | None = None) -> Path:
    """Owner-specified durable path: `$HOME/.cache/slipfork/do-vm/id_ed25519` and nothing else."""
    base = Path.home() if home is None else home
    return base / ".cache" / "slipfork" / "do-vm" / "id_ed25519"


def identity_path(raw: str | Path | None, home: Path | None = None) -> Path:
    return default_identity_path(home) if raw is None else Path(raw).expanduser()


def _key_body(public_key: str) -> str:
    fields = public_key.split()
    if len(fields) < 2:
        raise HarnessError(f"not a public key: {public_key!r}")
    return fields[1]


def _materialize_identity(path: Path) -> None:
    parent = path.parent
    parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(parent, 0o700)
    if path.exists():
        os.chmod(path, 0o600)
        return
    _run(
        [
            "ssh-keygen",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "slipfork-do-vm",
            "-f",
            str(path),
        ]
    )
    os.chmod(path, 0o600)


def load_identity(
    raw: str | Path | None,
    *,
    materialize: bool = False,
    home: Path | None = None,
) -> Identity:
    path = identity_path(raw, home)
    if materialize:
        _materialize_identity(path)
    if not path.is_file():
        raise HarnessError(
            f"identity {path} does not exist; run `tools/do_vm.py up` to materialize it"
        )
    os.chmod(path, 0o600)
    derived = _run(["ssh-keygen", "-y", "-P", "", "-f", str(path)], check=False)
    if derived.returncode != 0:
        raise HarnessError(
            f"cannot use identity {path} without a passphrase: {_detail(derived)}; the "
            "harness SSH calls run with IdentityAgent=none and BatchMode=yes"
        )
    public_key = derived.stdout.strip()
    if not public_key:
        raise HarnessError(f"ssh-keygen -y returned no public key for {path}")
    public_key_file = Path(f"{path}.pub")
    if public_key_file.is_file():
        text = public_key_file.read_text(encoding="utf-8")
        if _key_body(text) != _key_body(public_key):
            raise HarnessError(
                f"{public_key_file} does not match the private key {path}; delete the stale "
                ".pub file and re-run `tools/do_vm.py up`"
            )
    else:
        public_key_file.write_text(f"{public_key}\n", encoding="utf-8")
        os.chmod(public_key_file, 0o644)
    return Identity(path=path, public_key=public_key, public_key_file=public_key_file)


def _visible_key_id(keys: list[dict[str, Any]], body: str) -> str | None:
    for key in keys:
        public_key = key.get("public_key")
        if not isinstance(public_key, str):
            continue
        try:
            if _key_body(public_key) == body:
                return str(key["id"])
        except HarnessError:
            continue
    return None


def _wait_until(
    probe: Callable[[], T | None],
    *,
    timeout_seconds: int,
    on_timeout: Callable[[], HarnessError],
) -> T:
    """Poll `probe` until it returns a value; raise `on_timeout()` at the deadline."""
    deadline = time.monotonic() + timeout_seconds
    while True:
        value = probe()
        if value is not None:
            return value
        if time.monotonic() >= deadline:
            raise on_timeout()
        time.sleep(READY_POLL_SECONDS)


def _wait_for_account_key(doctl: Doctl, identity: Identity, body: str) -> str:
    """Bound the registry's propagation lag after a successful import."""

    def on_timeout() -> HarnessError:
        return HarnessError(
            f"doctl reported a successful import of {identity.public_key_file} but the key "
            f"is not visible after {ACCOUNT_KEY_TIMEOUT_SECONDS} seconds; re-run "
            "`tools/do_vm.py up` once the account registry catches up"
        )

    return _wait_until(
        lambda: _visible_key_id(doctl.ssh_keys(), body),
        timeout_seconds=ACCOUNT_KEY_TIMEOUT_SECONDS,
        on_timeout=on_timeout,
    )


def ensure_account_key(doctl: Doctl, identity: Identity) -> str:
    """Resolve the bootstrap key against the account, importing it when `doctl` cannot see it."""
    body = _key_body(identity.public_key)
    keys = doctl.ssh_keys()
    key_id = _visible_key_id(keys, body)
    if key_id is not None:
        return key_id
    name = f"slipfork-{hashlib.sha256(body.encode()).hexdigest()[:16]}"
    imported = _run(
        [
            "doctl",
            "compute",
            "ssh-key",
            "import",
            name,
            "--public-key-file",
            str(identity.public_key_file),
        ],
        check=False,
    )
    if imported.returncode == 0:
        return _wait_for_account_key(doctl, identity, body)
    keys = doctl.ssh_keys()
    key_id = _visible_key_id(keys, body)
    if key_id is not None:
        return key_id
    visible = (
        ", ".join(f"{key.get('name')} (id={key.get('id')})" for key in keys) or "none"
    )
    detail = _detail(imported) or "the import failed without stderr"
    raise HarnessError(
        f"the identity {identity.path} is not visible to this doctl context and could not be "
        f"imported ({detail}); visible keys: {visible}"
    )


def _read_authorized_keys(paths: list[str]) -> list[str]:
    chunks: list[str] = []
    for raw in paths:
        path = Path(raw)
        try:
            text = path.read_text(encoding="utf-8")
        except OSError as error:
            raise HarnessError(
                f"cannot read --authorized-key {raw}: {error}"
            ) from error
        if not text.strip():
            raise HarnessError(f"--authorized-key {raw} is empty")
        chunks.append(text if text.endswith("\n") else f"{text}\n")
    return chunks


def _authorized_keys_blob(
    identity: Identity, extra_identities: list[Identity], extra_chunks: list[str]
) -> str:
    chunks = [f"{identity.public_key.strip()}\n"]
    for other in extra_identities:
        chunks.append(f"{other.public_key.strip()}\n")
    chunks.extend(extra_chunks)
    return "".join(chunks)


def _install_authorized_keys(
    blob: str, droplet: Droplet, known_hosts: Path, identity: Identity
) -> None:
    remote = f"bash -c {shlex.quote(render_install_authorized_keys())}"
    _run(
        ssh_argv(droplet, known_hosts, identity, remote),
        input_text=blob,
    )


def _print_fingerprints(
    droplet: Droplet, known_hosts: Path, identity: Identity
) -> None:
    files = " ".join(authorized_keys_paths())
    remote = (
        f"for _file in {files}; do "
        'printf "== %s\\n" "$_file"; '
        'ssh-keygen -lf "$_file"; done'
    )
    result = _run(ssh_argv(droplet, known_hosts, identity, remote), check=False)
    if result.returncode != 0:
        raise HarnessError(
            f"could not read authorized-key fingerprints from {droplet.ip}: "
            f"{_detail(result)}"
        )
    print("keys     authorized_keys fingerprints:")
    for line in result.stdout.splitlines():
        print(f"         {line}")


def _confirm(question: str, assume_yes: bool) -> None:
    if assume_yes:
        return
    if not sys.stdin.isatty():
        raise HarnessError(
            f"{question}: refusing without confirmation in a non-interactive shell; "
            "re-run with --yes"
        )
    if input(f"{question} [y/N] ").strip().lower() not in {"y", "yes"}:
        raise HarnessError("aborted by user")


def _at_most_one(label: str, items: list[T]) -> T | None:
    if len(items) > 1:
        raise HarnessError(
            f"{len(items)} {label}s carry tag {TAG}; run `tools/do_vm.py down --yes`"
        )
    return items[0] if items else None


def _exactly_one(label: str, items: list[T]) -> T:
    if len(items) != 1:
        raise HarnessError(
            f"expected exactly one {label} tagged {TAG} after create, found {len(items)}"
        )
    return items[0]


def _single_droplet(doctl: Doctl) -> Droplet:
    droplet = _at_most_one("droplet", doctl.tagged_droplets())
    if droplet is None:
        raise HarnessError(f"no droplet tagged {TAG}; run `tools/do_vm.py up`")
    return droplet


def _cost_line(size: Size, region: str, volume_gb: int) -> str:
    if size.price_monthly is None:
        raise HarnessError(
            f"doctl reports no monthly price for size '{size.slug}'; refusing to create a "
            "droplet whose cost cannot be shown"
        )
    volume_cost = VOLUME_USD_PER_GIB_MONTH * volume_gb
    volume = f"${volume_cost:.2f}/month volume ({volume_gb} GiB, estimate)"
    droplet = f"${size.price_monthly:.2f}/month droplet ({size.slug}, {region})"
    total = f"${size.price_monthly + volume_cost:.2f}/month (estimate)"
    return f"{droplet} + {volume} = {total}"


def _create_volume(doctl: Doctl, region: str, volume_gb: int) -> Volume:
    _run(
        [
            "doctl",
            "compute",
            "volume",
            "create",
            TAG,
            "--region",
            region,
            "--size",
            f"{volume_gb}GiB",
            "--desc",
            "Slipfork dev VM Btrfs volume (disposable; tools/do_vm.py down --delete-volume)",
            "--tag",
            TAG,
        ]
    )
    volumes = doctl.tagged_volumes()
    return _exactly_one("volume", volumes)


def _create_droplet(
    doctl: Doctl,
    region: str,
    size: str,
    key_id: str,
    volume: Volume,
    *,
    allow_format: bool,
) -> Droplet:
    _run(
        [
            "doctl",
            "compute",
            "droplet",
            "create",
            TAG,
            "--image",
            IMAGE_SLUG,
            "--size",
            size,
            "--region",
            region,
            "--ssh-keys",
            key_id,
            "--tag-name",
            TAG,
            "--volumes",
            volume.id,
            "--user-data",
            cloud_init(ProvisionConfig(allow_format=allow_format)),
            "--wait",
            "--wait-timeout",
            "10m",
        ]
    )
    droplets = doctl.tagged_droplets()
    return _exactly_one("droplet", droplets)


def _attach_volume(droplet: Droplet, volume: Volume) -> None:
    _run(
        [
            "doctl",
            "compute",
            "volume-action",
            "attach",
            volume.id,
            str(droplet.id),
            "--wait",
            "--wait-timeout",
            "10m",
        ]
    )


def _wait_volume_detached(doctl: Doctl, volume_id: str) -> None:
    def probe() -> bool | None:
        current = next(
            (item for item in doctl.tagged_volumes() if item.id == volume_id), None
        )
        if current is None:
            raise HarnessError(f"volume {volume_id} disappeared while detaching")
        return True if not current.droplet_ids else None

    def on_timeout() -> HarnessError:
        current = next(
            (item for item in doctl.tagged_volumes() if item.id == volume_id), None
        )
        attached = (
            ", ".join(str(item) for item in sorted(current.droplet_ids))
            if current is not None
            else "unknown"
        )
        return HarnessError(
            f"volume {volume_id} is still attached to {attached} after "
            f"{DETACH_TIMEOUT_SECONDS // 60} minutes; refusing to destroy the droplet"
        )

    _wait_until(probe, timeout_seconds=DETACH_TIMEOUT_SECONDS, on_timeout=on_timeout)


def _wait_no_tagged_droplet(doctl: Doctl) -> None:
    _wait_until(
        lambda: True if not doctl.tagged_droplets() else None,
        timeout_seconds=DROPLET_DELETE_TIMEOUT_SECONDS,
        on_timeout=lambda: HarnessError(
            f"a droplet tagged {TAG} survived deletion; run `tools/do_vm.py status`"
        ),
    )


def _recreate_droplet(
    doctl: Doctl,
    droplet: Droplet,
    volume: Volume,
    size: str,
    key_id: str,
    root: Path,
    *,
    allow_format: bool,
) -> Droplet:
    print(
        f"recreate droplet {droplet.id}: the identity cannot open it; volume {volume.id} is "
        "preserved and the droplet is replaced"
    )
    if volume.id in droplet.volume_ids:
        print(f"detach   volume {volume.id} from droplet {droplet.id}")
        _run(
            [
                "doctl",
                "compute",
                "volume-action",
                "detach",
                volume.id,
                str(droplet.id),
                "--wait",
                "--wait-timeout",
                "10m",
            ]
        )
        _wait_volume_detached(doctl, volume.id)
    _run(["doctl", "compute", "droplet", "delete", str(droplet.id), "--force"])
    known_hosts_for(root, droplet).unlink(missing_ok=True)
    _wait_no_tagged_droplet(doctl)
    return _create_droplet(
        doctl, droplet.region, size, key_id, volume, allow_format=allow_format
    )


def _is_auth_failure(detail: str) -> bool:
    return any(marker in detail for marker in SSH_AUTH_FAILURE)


def _is_host_key_failure(detail: str) -> bool:
    return any(marker in detail for marker in SSH_HOST_KEY_FAILURE)


def _raise_on_host_key(
    probe: subprocess.CompletedProcess[str], droplet: Droplet, known_hosts: Path
) -> None:
    detail = _detail(probe)
    if _is_host_key_failure(detail):
        raise HarnessError(
            f"SSH host-key check failed for {droplet.ip}; inspect {known_hosts} and delete it "
            "only if the droplet was recreated at the same address"
        )


def _transport_failure(
    probe: subprocess.CompletedProcess[str], droplet: Droplet, known_hosts: Path
) -> None:
    _raise_on_host_key(probe, droplet, known_hosts)
    detail = _detail(probe)
    if _is_auth_failure(detail):
        raise HarnessError(f"SSH authentication to {droplet.ip} was rejected: {detail}")
    raise HarnessError(f"cannot reach {droplet.ip} over SSH: {detail or 'no stderr'}")


def _ready_probe(
    droplet: Droplet, known_hosts: Path, identity: Identity
) -> subprocess.CompletedProcess[str]:
    return _run(
        ssh_argv(droplet, known_hosts, identity, f"test -f {MARKER}"), check=False
    )


def _cloud_init_running(
    droplet: Droplet, known_hosts: Path, identity: Identity
) -> bool:
    probe = _run(
        ssh_argv(droplet, known_hosts, identity, "cloud-init status"), check=False
    )
    return probe.returncode == 0 and "status: running" in (probe.stdout or "")


def _provision(
    droplet: Droplet,
    known_hosts: Path,
    identity: Identity,
    *,
    allow_format: bool,
) -> None:
    probe = _run(
        ssh_argv(droplet, known_hosts, identity, "bash -s"),
        input_text=render_provision_script(ProvisionConfig(allow_format=allow_format)),
        check=False,
    )
    if probe.returncode != 0:
        detail = _detail(probe)
        if (
            probe.returncode == 255
            or _is_host_key_failure(detail)
            or _is_auth_failure(detail)
        ):
            _transport_failure(probe, droplet, known_hosts)
        raise ProvisioningError(
            f"provisioning on {droplet.ip} failed ({probe.returncode}): {detail}",
            probe.returncode,
        )
    for line in (probe.stdout or "").splitlines():
        if line.startswith("slipfork: "):
            print(line)


def _run_remote_script(
    droplet: Droplet,
    known_hosts: Path,
    identity: Identity,
    label: str,
    script: str,
) -> subprocess.CompletedProcess[str]:
    """Run one self-contained script over SSH stdin; `label` names the step for logs."""
    return _run(
        ssh_argv(droplet, known_hosts, identity, f"bash -s {shlex.quote(label)}"),
        input_text=script,
        check=False,
    )


def _step_failed(
    step: str,
    result: subprocess.CompletedProcess[str],
    droplet: Droplet,
    known_hosts: Path,
) -> None:
    """Print a step's transcript and raise on failure (transport failures stay distinct)."""
    for line in (result.stdout or "").splitlines():
        print(line)
    if result.returncode == 0:
        return
    detail = _detail(result)
    if (
        result.returncode == 255
        or _is_host_key_failure(detail)
        or _is_auth_failure(detail)
    ):
        _transport_failure(result, droplet, known_hosts)
    raise HarnessError(f"{step} on {droplet.ip} failed ({result.returncode}): {detail}")


def _seed_demo(droplet: Droplet, known_hosts: Path, identity: Identity) -> None:
    """Seed `/home/slipfork/demo` from the tracked template once (`render_demo_seed`)."""
    _step_failed(
        "demo seeding",
        _run_remote_script(
            droplet, known_hosts, identity, "demo-seed", render_demo_seed()
        ),
        droplet,
        known_hosts,
    )


def parse_product_binaries(metadata_text: str) -> list[str]:
    """The sorted binary target names of [`PRODUCT_BUILD_PACKAGES`], from `cargo metadata`."""
    try:
        data = json.loads(metadata_text)
    except json.JSONDecodeError as error:
        raise HarnessError(f"cargo metadata did not emit JSON: {error}") from error
    packages = data.get("packages") if isinstance(data, dict) else None
    if not isinstance(packages, list):
        raise HarnessError("cargo metadata returned no `packages` list")
    names: list[str] = []
    for package in packages:
        if not isinstance(package, dict):
            raise HarnessError(
                f"cargo metadata package entry is malformed: {package!r}"
            )
        if package.get("name") not in PRODUCT_BUILD_PACKAGES:
            continue
        targets = package.get("targets")
        if not isinstance(targets, list):
            raise HarnessError(
                f"cargo metadata package {package.get('name')!r} has no `targets` list"
            )
        for target in targets:
            if not isinstance(target, dict):
                raise HarnessError(
                    f"cargo metadata target entry is malformed: {target!r}"
                )
            kind = target.get("kind")
            name = target.get("name")
            if isinstance(kind, list) and "bin" in kind and isinstance(name, str):
                names.append(name)
    if not names:
        raise HarnessError(
            "cargo metadata listed no binary targets for "
            + ", ".join(PRODUCT_BUILD_PACKAGES)
        )
    return sorted(names)


def product_build_command(binaries: list[str], profile: str) -> str:
    """One `cargo build` for exactly `binaries` in `profile`, from the metadata-derived list."""
    if profile not in PROFILE_BIN_PROFILES:
        raise HarnessError(
            f"unsupported product build profile '{profile}'; "
            f"expected one of {', '.join(PROFILE_BIN_PROFILES)}"
        )
    flags = " --release" if profile == "release" else ""
    packages = " ".join(f"-p {package}" for package in PRODUCT_BUILD_PACKAGES)
    targets = " ".join(f"--bin {shlex.quote(binary)}" for binary in binaries)
    return f"cargo build{flags} {packages} {targets}"


def _build_product(droplet: Droplet, known_hosts: Path, identity: Identity) -> None:
    """Build the product binaries in `BUILD_PROFILE`, derived from `cargo metadata`."""
    metadata = _run(
        ssh_argv(droplet, known_hosts, identity, suite_command(PRODUCT_METADATA)),
        check=False,
    )
    if metadata.returncode != 0:
        raise HarnessError(
            f"cargo metadata on {droplet.ip} failed ({metadata.returncode}): "
            f"{_detail(metadata)}"
        )
    binaries = parse_product_binaries(metadata.stdout)
    missing = [binary for binary in PRODUCT_BINARIES if binary not in binaries]
    unexpected = [binary for binary in binaries if binary not in PRODUCT_BINARIES]
    if missing or unexpected:
        raise HarnessError(
            "cargo metadata disagrees with the shipped binary set: missing "
            f"[{', '.join(missing)}], unexpected [{', '.join(unexpected)}]"
        )
    build = _run(
        ssh_argv(
            droplet,
            known_hosts,
            identity,
            suite_command(product_build_command(binaries, BUILD_PROFILE)),
        ),
        check=False,
    )
    _step_failed("product build", build, droplet, known_hosts)


def _install_product(droplet: Droplet, known_hosts: Path, identity: Identity) -> None:
    """Install the four binaries, then let `slipfork daemon install` own `:80`."""
    _step_failed(
        "product install",
        _run_remote_script(
            droplet, known_hosts, identity, "product-install", render_product_install()
        ),
        droplet,
        known_hosts,
    )


def _poll_or_raise(
    probe: Callable[[], subprocess.CompletedProcess[str]],
    *,
    droplet: Droplet,
    known_hosts: Path,
    timeout_seconds: int,
    message: Callable[[str], str],
    auth_grace_seconds: int = 0,
) -> None:
    """Poll until the probe succeeds; raise `message(last_detail)` at the deadline.

    Host-key changes abort immediately. Authentication rejections abort once the cold-boot
    grace has elapsed; before that they are treated as "sshd is not up yet".
    """
    start = time.monotonic()
    last = {"detail": ""}

    def attempt() -> bool | None:
        result = probe()
        if result.returncode == 0:
            return True
        detail = _detail(result)
        _raise_on_host_key(result, droplet, known_hosts)
        if _is_auth_failure(detail) and time.monotonic() - start >= auth_grace_seconds:
            _transport_failure(result, droplet, known_hosts)
        last["detail"] = detail
        return None

    _wait_until(
        attempt,
        timeout_seconds=timeout_seconds,
        on_timeout=lambda: HarnessError(message(last["detail"] or "no stderr")),
    )


def _wait_ssh(droplet: Droplet, known_hosts: Path, identity: Identity) -> None:
    _poll_or_raise(
        lambda: _run(ssh_argv(droplet, known_hosts, identity, "true"), check=False),
        droplet=droplet,
        known_hosts=known_hosts,
        timeout_seconds=SSH_WAIT_TIMEOUT_SECONDS,
        auth_grace_seconds=COLD_BOOT_GRACE_SECONDS,
        message=lambda detail: (
            f"SSH did not become reachable at {droplet.ip} within "
            f"{SSH_WAIT_TIMEOUT_SECONDS // 60} minutes; last probe: {detail}"
        ),
    )


def _wait_ready(root: Path, droplet: Droplet, identity: Identity) -> None:
    known_hosts = known_hosts_for(root, droplet)

    def on_timeout(detail: str) -> str:
        print(
            f"error: cloud-init did not write {MARKER} within "
            f"{READY_TIMEOUT_SECONDS // 60} minutes; last cloud-init output:",
            file=sys.stderr,
        )
        for remote in (
            "tail -n 100 /var/log/cloud-init-output.log",
            "journalctl -u cloud-init --no-pager -n 80",
        ):
            logs = _run(ssh_argv(droplet, known_hosts, identity, remote), check=False)
            print(f"$ ssh root@{droplet.ip} {remote}", file=sys.stderr)
            print((logs.stdout or "") + (logs.stderr or ""), file=sys.stderr)
        return (
            f"cloud-init provisioning on {droplet.ip} did not finish; last probe: "
            f"{detail}; inspect the log above; "
            "run `tools/do_vm.py down --yes` to stop billing now"
        )

    _poll_or_raise(
        lambda: _ready_probe(droplet, known_hosts, identity),
        droplet=droplet,
        known_hosts=known_hosts,
        timeout_seconds=READY_TIMEOUT_SECONDS,
        auth_grace_seconds=COLD_BOOT_GRACE_SECONDS,
        message=on_timeout,
    )


def _print_summary(
    droplet: Droplet, volume: Volume, cost: str, identity: Identity
) -> None:
    print(
        f"droplet  {droplet.name}  id={droplet.id}  region={droplet.region}  ip={droplet.ip}"
    )
    print(
        f"volume   {volume.name}  id={volume.id}  region={volume.region}  "
        f"{volume.size_gb} GiB"
    )
    print(f"btrfs    {STATE_DIR} mounted with {MOUNT_OPTIONS}; marker {MARKER}")
    print(f"identity {identity.path}")
    print(f"ssh      ssh -i {identity.path} root@{droplet.ip}")
    print(f"cost     {cost}")
    print("next     tools/do_vm.py sync && tools/do_vm.py test")
    print(
        "down     tools/do_vm.py down --yes   "
        "(keeps the volume; add --delete-volume to drop it)"
    )


def _resolve_size(doctl: Doctl, slug: str, region: str) -> Size:
    size = doctl.size(slug)
    if size is None:
        raise HarnessError(
            f"size '{slug}' is not offered; run `doctl compute size list`"
        )
    if size.regions and region not in size.regions:
        raise HarnessError(
            f"size '{slug}' is not offered in region '{region}'; offered in: "
            + ", ".join(size.regions)
        )
    return size


def cmd_up(args: argparse.Namespace) -> int:
    if args.volume_gb < MIN_VOLUME_GB:
        raise HarnessError(f"--volume-gb must be at least {MIN_VOLUME_GB}")
    root = args.root
    bootstrap = load_identity(None, materialize=True, home=args.home)
    additions = [load_identity(item, home=args.home) for item in args.identity]
    extra_chunks = _read_authorized_keys(args.authorized_key)
    doctl = args.doctl
    droplets = doctl.tagged_droplets()
    volumes = doctl.tagged_volumes()
    droplet = _at_most_one("droplet", droplets)
    volume = _at_most_one("volume", volumes)

    for label, existing in (("droplet", droplet), ("volume", volume)):
        if existing is not None and existing.region != args.region:
            raise HarnessError(
                f"tagged {label} {existing.id} is in {existing.region}, not {args.region}; "
                "run `tools/do_vm.py down --yes` before changing regions"
            )
    if args.region not in (regions := doctl.regions()):
        raise HarnessError(
            f"region '{args.region}' is not available; available regions: "
            + ", ".join(regions)
        )

    size = _resolve_size(
        doctl, droplet.size if droplet is not None else args.size, args.region
    )
    volume_gb = volume.size_gb if volume is not None else args.volume_gb
    cost = _cost_line(size, args.region, volume_gb)

    need_droplet = droplet is None
    need_volume = volume is None
    already_attached = (
        droplet is not None and volume is not None and volume.id in droplet.volume_ids
    )
    need_attach = droplet is not None and volume is not None and not already_attached
    recreate = False
    if droplet is not None:
        known_hosts = known_hosts_for(root, droplet)
        probe = _ready_probe(droplet, known_hosts, bootstrap)
        if probe.returncode != 0:
            _raise_on_host_key(probe, droplet, known_hosts)
            recreate = _is_auth_failure(_detail(probe))

    if droplet is not None or volume is not None:
        print(
            "note: existing tagged droplet/volume win; --size and --volume-gb apply only "
            "to newly created resources"
        )
    if recreate:
        print(
            f"note: identity {bootstrap.path} cannot open droplet {droplet.id}; `up` will "
            "detach the volume, destroy the droplet, and recreate it on the same volume"
        )
    elif droplet is not None and not need_volume and not need_attach:
        print(f"reusing droplet {droplet.id} ({droplet.ip}) and volume {volume.id}")
    elif droplet is not None and not need_volume:
        print(
            f"reusing droplet {droplet.id} ({droplet.ip}) and volume {volume.id}; "
            "the volume is not attached"
        )
    else:
        print(f"region   {args.region}")
        print(
            f"size     {size.slug} ({size.vcpus or '?'} vCPU, "
            f"{size.memory_mib or '?'} MiB RAM, {size.disk_gib or '?'} GiB SSD)"
        )
        if need_volume:
            print(f"volume   create {args.volume_gb} GiB, tag {TAG}")
        else:
            if volume is None:
                raise HarnessError("volume reuse branch lost its tagged volume")
            print(f"volume   reuse {volume.id} ({volume.size_gb} GiB)")
        if need_droplet:
            print(f"image    {IMAGE_SLUG}")
            print(f"key      bootstrap identity {bootstrap.path}")
        print(f"cost     {cost}")
        print("down     tools/do_vm.py down --yes")

    if need_droplet or need_volume or need_attach or recreate:
        action = (
            "destroy and recreate the tagged droplet on its volume"
            if recreate
            else "create/attach the missing tagged resources"
        )
        _confirm(f"{action} in {args.region} for this cost?", args.yes)

    key_id = ensure_account_key(doctl, bootstrap)
    allow_format = args.format_blank

    created_droplet = False
    created_volume = False
    attached_volume = False
    try:
        if need_volume:
            volume = _create_volume(doctl, args.region, args.volume_gb)
            created_volume = True
            # A volume this invocation created cannot hold foreign data.
            allow_format = True
        if volume is None:
            raise HarnessError("no tagged volume available after create/reuse")
        if recreate:
            if droplet is None:
                raise HarnessError("recreate branch lost its tagged droplet")
            droplet = _recreate_droplet(
                doctl,
                droplet,
                volume,
                size.slug,
                key_id,
                root,
                allow_format=allow_format,
            )
            created_droplet = True
        elif need_droplet:
            droplet = _create_droplet(
                doctl,
                args.region,
                size.slug,
                key_id,
                volume,
                allow_format=allow_format,
            )
            created_droplet = True
        elif not already_attached:
            _attach_volume(droplet, volume)
            attached_volume = True
    except HarnessError as error:
        raise HarnessError(
            f"{error}\nnote: a partially created tagged resource may exist; "
            "run `tools/do_vm.py status` and `tools/do_vm.py down --yes` to clean up"
        ) from error

    if droplet is None:
        raise HarnessError("no tagged droplet available after create/reuse")
    known_hosts = known_hosts_for(root, droplet)
    if not created_droplet:
        probe = _ready_probe(droplet, known_hosts, bootstrap)
        if probe.returncode != 0:
            _raise_on_host_key(probe, droplet, known_hosts)
            if _is_auth_failure(_detail(probe)):
                _transport_failure(probe, droplet, known_hosts)
            if (created_volume or attached_volume) or not _cloud_init_running(
                droplet, known_hosts, bootstrap
            ):
                _wait_ssh(droplet, known_hosts, bootstrap)
                _provision(droplet, known_hosts, bootstrap, allow_format=allow_format)
    _wait_ready(root, droplet, bootstrap)

    blob = _authorized_keys_blob(bootstrap, additions, extra_chunks)
    try:
        _install_authorized_keys(blob, droplet, known_hosts, bootstrap)
    except HarnessError as error:
        raise HarnessError(
            f"{error}\nrun `tools/do_vm.py down --yes` to stop billing now"
        ) from error
    _print_summary(droplet, volume, cost, bootstrap)
    try:
        _print_fingerprints(droplet, known_hosts, bootstrap)
    except HarnessError as error:
        raise HarnessError(
            f"{error}\nrun `tools/do_vm.py down --yes` to stop billing now"
        ) from error
    return 0


def cmd_status(args: argparse.Namespace) -> int:
    root = args.root
    identity = load_identity(
        args.identity[0] if args.identity else None, home=args.home
    )
    doctl = args.doctl
    droplets = doctl.tagged_droplets()
    volumes = doctl.tagged_volumes()
    if not droplets:
        print(f"droplet  none (tag {TAG})")
    for droplet in droplets:
        print(
            f"droplet  {droplet.name}  id={droplet.id}  region={droplet.region}  "
            f"size={droplet.size}  ip={droplet.ip}"
        )
    if not volumes:
        print(f"volume   none (tag {TAG})")
    for volume in volumes:
        print(
            f"volume   {volume.name}  id={volume.id}  region={volume.region}  "
            f"{volume.size_gb} GiB"
        )
    for droplet in droplets:
        _print_fingerprints(droplet, known_hosts_for(root, droplet), identity)
    if len(droplets) == 1:
        print(f"ssh      ssh -i {identity.path} root@{droplets[0].ip}")
        print("sync     tools/do_vm.py sync")
        print("down     tools/do_vm.py down --yes")
    return 0


def _git_files(root: Path, *args: str) -> list[PurePosixPath]:
    result = _run(["git", "ls-files", *args, "-z"], cwd=root)
    return [PurePosixPath(raw) for raw in result.stdout.split("\0") if raw]


def _pruned(paths: list[PurePosixPath]) -> list[PurePosixPath]:
    return [
        path for path in paths if path.parts and path.parts[0] not in EXCLUDED_TOP_LEVEL
    ]


def _refuse_secrets(paths: list[PurePosixPath]) -> None:
    secrets = [str(path) for path in paths if guard.is_secret_env(path)]
    if secrets:
        raise HarnessError(
            "refusing to sync secret environment file(s): "
            + ", ".join(sorted(secrets))
            + " (only .env.example may be synced)"
        )


def _stream_tar(root: Path, paths: list[PurePosixPath], ssh: list[str]) -> None:
    process = _run_stream(ssh)
    if process.stdin is None or process.stderr is None:
        raise HarnessError("could not open SSH stdin/stderr pipes")
    failure: OSError | tarfile.TarError | None = None
    try:
        with tarfile.open(fileobj=process.stdin, mode="w|") as archive:
            for relative in paths:
                absolute = root / relative
                if absolute.is_symlink() or absolute.exists():
                    archive.add(absolute, arcname=str(relative), recursive=False)
    except (OSError, tarfile.TarError) as error:
        failure = error
    finally:
        try:
            process.stdin.close()
        except OSError as error:
            failure = failure or error
    stderr = process.stderr.read().decode("utf-8", "replace").strip()
    returncode = process.wait()
    if returncode != 0 or failure is not None:
        detail = str(failure) if failure is not None else stderr or "no stderr"
        raise HarnessError(
            f"`ssh` tar-pipe into {REMOTE_STAGING} failed ({returncode}): {detail}"
        )


def cmd_sync(args: argparse.Namespace) -> int:
    root = args.root
    identity = load_identity(None, home=args.home)
    droplet = _single_droplet(args.doctl)
    sync_paths = _pruned(_git_files(root, "--cached", "--others", "--exclude-standard"))
    _refuse_secrets(sync_paths)
    known_hosts = known_hosts_for(root, droplet)
    _run(
        ssh_argv(
            droplet,
            known_hosts,
            identity,
            f"rm -rf {REMOTE_STAGING} && mkdir -p {REMOTE_STAGING}",
        )
    )
    _stream_tar(
        root,
        sync_paths,
        ssh_argv(droplet, known_hosts, identity, f"tar -x -C {REMOTE_STAGING}"),
    )
    replace = (
        f"set -e; "
        f"if [ -d {REMOTE_TREE}/target ]; then "
        f"mv {REMOTE_TREE}/target {REMOTE_STAGING}/target; fi; "
        f"rm -rf {REMOTE_TREE}; mv {REMOTE_STAGING} {REMOTE_TREE}; "
        f"chown -R {REMOTE_USER}:{REMOTE_USER} {REMOTE_TREE}"
    )
    _run(ssh_argv(droplet, known_hosts, identity, replace))
    _run(
        ssh_argv(
            droplet,
            known_hosts,
            identity,
            f"install -d -o {REMOTE_USER} -g {REMOTE_USER} -m 700 {SUITE_TEST_ROOT}",
        )
    )
    _seed_demo(droplet, known_hosts, identity)
    _build_product(droplet, known_hosts, identity)
    _install_product(droplet, known_hosts, identity)
    print(f"synced {len(sync_paths)} file(s) to {REMOTE_TREE} on {droplet.ip}")
    return 0


def suite_command(command: str) -> str:
    """One fixed envelope for every unprivileged remote suite command."""
    inner = (
        f"cd {REMOTE_TREE} && exec env -i "
        f"HOME={SUITE_HOME} TMPDIR={SUITE_HOME}/tmp "
        f"SLIPFORK_TEST_ROOT={SUITE_TEST_ROOT} "
        f"CARGO_HOME={REMOTE_HOME}/.cargo RUSTUP_HOME={REMOTE_HOME}/.rustup "
        f"PATH={REMOTE_HOME}/.cargo/bin:/usr/local/bin:/usr/bin:/bin "
    ) + command
    return f"/usr/sbin/runuser -u {REMOTE_USER} -- bash -c {shlex.quote(inner)}"


def owner_envelope(command: str) -> str:
    """Owner SSH envelope for forwarded commands: state on the volume, no login shell."""
    return (
        f"if [ -d {REMOTE_TREE} ]; then cd {REMOTE_TREE}; fi; "
        f"export HOME={OWNER_HOME} TMPDIR={OWNER_HOME}/tmp; "
        f"export CARGO_HOME={REMOTE_HOME}/.cargo RUSTUP_HOME={REMOTE_HOME}/.rustup; "
        f"export PATH={REMOTE_TREE}/target/debug:{REMOTE_HOME}/.cargo/bin:"
        "/usr/local/bin:/usr/bin:/bin; "
        f"{command}"
    )


def owner_shell_envelope() -> str:
    """Interactive owner shell: the same envelope plus the built-CLI assertion."""
    return owner_envelope(
        "command -v slipfork >/dev/null || "
        "{ echo 'target/debug/slipfork missing; run tools/do_vm.py test first' >&2; "
        "exit 1; }; "
        "exec bash --noprofile --norc"
    )


def _parse_cargo_version(output: str) -> tuple[int, ...]:
    for token in output.split():
        parts = token.split(".")
        if len(parts) == 3 and all(part.isdigit() for part in parts):
            return tuple(int(part) for part in parts)
    raise HarnessError(f"cannot parse a cargo version from: {output.strip()!r}")


def cmd_test(args: argparse.Namespace) -> int:
    raw_identity = args.identity[0] if args.identity else None
    root = args.root
    missing = [
        f"{path} ({node})" for _, _, path, node in SUITES if not (root / path).is_file()
    ]
    if missing:
        raise HarnessError(
            "missing live Btrfs conformance suite(s): "
            + ", ".join(missing)
            + "; the VM suites cannot run yet"
        )
    identity = load_identity(raw_identity, home=args.home)
    droplet = _single_droplet(args.doctl)
    known_hosts = known_hosts_for(root, droplet)

    marker = _ready_probe(droplet, known_hosts, identity)
    if marker.returncode != 0:
        if marker.returncode == 255:
            _transport_failure(marker, droplet, known_hosts)
        raise HarnessError(
            f"the droplet is not provisioned: {MARKER} is absent; run `tools/do_vm.py up`"
        )
    for _, _, path, _ in SUITES:
        remote = f"{REMOTE_TREE}/{path}"
        probe = _run(
            ssh_argv(droplet, known_hosts, identity, f"test -f {remote}"), check=False
        )
        if probe.returncode != 0:
            if probe.returncode == 255:
                _transport_failure(probe, droplet, known_hosts)
            raise HarnessError(
                f"missing live Btrfs conformance suite {remote} on the VM; "
                "run `tools/do_vm.py sync` first"
            )

    cargo = _run(
        ssh_argv(
            droplet,
            known_hosts,
            identity,
            f"{REMOTE_HOME}/.cargo/bin/cargo --version",
        ),
        check=False,
    )
    if cargo.returncode != 0:
        raise HarnessError(
            "cargo is not installed on the VM; provisioning is incomplete — re-run "
            "`tools/do_vm.py up`"
        )
    version = _parse_cargo_version(cargo.stdout)
    required = tuple(int(part) for part in RUST_MIN_VERSION.split("."))
    if version < required:
        raise HarnessError(
            f"cargo {'.'.join(map(str, version))} is older than {RUST_MIN_VERSION}; "
            "provisioning is incomplete — re-run `tools/do_vm.py up`"
        )

    docker = _run(
        ssh_argv(droplet, known_hosts, identity, suite_command("docker info")),
        check=False,
    )
    if docker.returncode != 0:
        raise HarnessError(
            f"docker did not answer `docker info` as {REMOTE_USER}: {_detail(docker)}"
        )

    _build_product(droplet, known_hosts, identity)
    _install_product(droplet, known_hosts, identity)

    for package, test, _, _ in SUITES:
        command = f"cargo test -p {package} --test {test} -- --test-threads=1"
        result = _run(
            ssh_argv(droplet, known_hosts, identity, suite_command(command)),
            capture=False,
            check=False,
        )
        if result.returncode != 0:
            return result.returncode
    return 0


def cmd_ssh(args: argparse.Namespace) -> int:
    root = args.root
    identity = load_identity(
        args.identity[0] if args.identity else None, home=args.home
    )
    droplet = _single_droplet(args.doctl)
    forwarded = list(args.remote)
    if forwarded and forwarded[0] == "--":
        forwarded = forwarded[1:]
    known_hosts = known_hosts_for(root, droplet)
    if forwarded:
        remote = owner_envelope(" ".join(shlex.quote(arg) for arg in forwarded))
        argv = ssh_argv(droplet, known_hosts, identity, remote, batch=False)
    else:
        argv = ssh_argv(
            droplet,
            known_hosts,
            identity,
            owner_shell_envelope(),
            batch=False,
            tty=True,
        )
    try:
        os.execvp("ssh", argv)
    except OSError as error:
        raise HarnessError(f"could not exec ssh: {error}") from error
    return 0


def cmd_down(args: argparse.Namespace) -> int:
    root = args.root
    doctl = args.doctl
    droplets = doctl.tagged_droplets()
    volumes = doctl.tagged_volumes()
    if not droplets and not volumes:
        print(f"nothing to destroy: no resources tagged {TAG}")
        return 0
    print(
        f"destroying {len(droplets)} droplet(s) tagged {TAG} and "
        f"{len(volumes) if args.delete_volume else 0} volume(s):"
    )
    for droplet in droplets:
        print(f"  droplet {droplet.name} id={droplet.id} ip={droplet.ip}")
    if args.delete_volume:
        for volume in volumes:
            print(f"  volume  {volume.name} id={volume.id} {volume.size_gb} GiB")
    else:
        for volume in volumes:
            print(
                f"  volume  {volume.name} id={volume.id} {volume.size_gb} GiB (kept; "
                "pass --delete-volume to destroy it)"
            )
    _confirm("destroy these tagged resources permanently?", args.yes)
    for droplet in droplets:
        _run(["doctl", "compute", "droplet", "delete", str(droplet.id), "--force"])
        known_hosts_for(root, droplet).unlink(missing_ok=True)
    if args.delete_volume:
        for volume in volumes:
            _run(["doctl", "compute", "volume", "delete", volume.id, "--force"])

    def probe() -> bool | None:
        remaining_droplets = doctl.tagged_droplets()
        remaining_volumes = doctl.tagged_volumes() if args.delete_volume else []
        return True if not remaining_droplets and not remaining_volumes else None

    def on_timeout() -> HarnessError:
        remaining_droplets = doctl.tagged_droplets()
        remaining_volumes = doctl.tagged_volumes() if args.delete_volume else []
        survivors = [
            *(f"droplet {item.id}" for item in remaining_droplets),
            *(f"volume {item.id}" for item in remaining_volumes),
        ]
        return HarnessError(
            f"resources tagged {TAG} survived deletion after "
            f"{DOWN_POLL_TIMEOUT_SECONDS} seconds: {', '.join(survivors)}"
        )

    _wait_until(probe, timeout_seconds=DOWN_POLL_TIMEOUT_SECONDS, on_timeout=on_timeout)
    print(
        f"destroyed {len(droplets)} droplet(s) and "
        f"{len(volumes) if args.delete_volume else 0} volume(s)"
    )
    if not args.delete_volume and volumes:
        print(
            f"volume   kept {len(volumes)} tagged volume(s); they still bill until "
            "`tools/do_vm.py down --yes --delete-volume`"
        )
    return 0


def _add_identity(
    parser: argparse.ArgumentParser, *, home: Path, for_up: bool = False
) -> None:
    default = default_identity_path(home)
    if for_up:
        help_text = (
            "additional SSH public key installed for root and slipfork (repeatable); the "
            f"default {default} is always the transport identity"
        )
    else:
        help_text = f"SSH private key (default: {default}); repeatable, the first listed path is used"
    parser.add_argument(
        "--identity",
        action="append",
        default=[],
        metavar="PATH",
        help=help_text,
    )


def build_parser(home: Path) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="tools/do_vm.py",
        description=(
            "DigitalOcean dev-VM harness: Btrfs volume, docker/Rust, :80 socket activation."
        ),
        epilog="See docs/design/do-vm-harness.md.",
    )
    commands = parser.add_subparsers(dest="command", required=True, metavar="command")

    up = commands.add_parser(
        "up", help="create or reconcile the tagged droplet + Btrfs volume"
    )
    up.add_argument(
        "--region",
        default=DEFAULT_REGION,
        help=f"DigitalOcean region (default: {DEFAULT_REGION})",
    )
    up.add_argument(
        "--size",
        default=DEFAULT_SIZE,
        help=f"droplet size slug (default: {DEFAULT_SIZE})",
    )
    up.add_argument(
        "--volume-gb",
        type=int,
        default=DEFAULT_VOLUME_GB,
        help=f"block-storage volume size in GiB (default: {DEFAULT_VOLUME_GB})",
    )
    _add_identity(up, home=home, for_up=True)
    up.add_argument(
        "--authorized-key",
        action="append",
        default=[],
        metavar="PATH",
        help=(
            "install this public-key file for root and slipfork after boot (repeatable; "
            "idempotent via sort -u)"
        ),
    )
    up.add_argument(
        "--format-blank",
        action="store_true",
        help=(
            "format a pre-existing blank volume after proving its first 8 MiB are all zero; "
            "DESTROYS any data on the volume; never overrides a detected foreign signature"
        ),
    )
    up.add_argument(
        "--yes", action="store_true", help="skip the cost confirmation prompt"
    )
    up.set_defaults(handler=cmd_up)

    status = commands.add_parser(
        "status", help="report tagged droplet and volume state"
    )
    _add_identity(status, home=home)
    status.set_defaults(handler=cmd_status)

    sync = commands.add_parser(
        "sync",
        help="sync the tree to the VM, seed the demo, install the product",
        description=(
            "Replace the remote tree, seed support/demo once, build the product binaries "
            "(derived from `cargo metadata`), install them into /usr/local/bin, and run "
            "`slipfork daemon install` so slipforkd.socket owns :80. "
            "Disclosure: this stops slipforkd.socket/slipforkd.service and `daemon install` hands "
            "the control socket over from any running slipforkd, so every running world's "
            "processes stop until the next `slipfork up`."
        ),
    )
    sync.set_defaults(handler=cmd_sync)

    test = commands.add_parser(
        "test",
        help="stream the live Btrfs kernel conformance suites",
        description=(
            "Build and install the product (same steps and the same daemon stop/handover "
            "disclosure as `sync`), then stream the five live Btrfs suites one at a time."
        ),
    )
    _add_identity(test, home=home)
    test.set_defaults(handler=cmd_test)

    ssh = commands.add_parser("ssh", help="open an interactive root SSH session")
    _add_identity(ssh, home=home)
    ssh.add_argument(
        "remote",
        nargs=argparse.REMAINDER,
        metavar="-- <remote command>",
        help="run this command in the owner envelope instead of an interactive shell",
    )
    ssh.set_defaults(handler=cmd_ssh)

    down = commands.add_parser(
        "down", help="destroy the tagged droplet (keeps the volume by default)"
    )
    down.add_argument(
        "--yes", action="store_true", help="skip the destructive confirmation prompt"
    )
    down.add_argument(
        "--delete-volume",
        action="store_true",
        help="also destroy the tagged Btrfs volume and its worlds",
    )
    down.set_defaults(handler=cmd_down)
    return parser


def main(argv: list[str] | None = None) -> int:
    home = Path.home()
    args = build_parser(home).parse_args(argv)
    try:
        args.doctl = Doctl()
        args.root = repo_root()
        args.home = home
        return int(args.handler(args))
    except ProvisioningError as error:
        print(f"error: {error}", file=sys.stderr)
        return error.returncode
    except HarnessError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
