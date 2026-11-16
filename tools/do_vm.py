#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""DigitalOcean dev-VM harness: droplet + ZFS volume + Incus pool (`M3.5`).

Provisions a throwaway Ubuntu 24.04 droplet, attaches a 100 GiB block-storage volume, formats it
as a ZFS pool, installs Incus, and registers that pool as Incus storage `default`. The VM runs
Incus **system containers (LXC)** only: DigitalOcean droplets expose no nested KVM.

The harness is deliberately two-phase: `up` never destroys anything, so a human can log in and
try the VM; `down --yes` is the only destructive command.

  tools/do_vm.py up      [--region ams3] [--size s-4vcpu-8gb] [--volume-gb 100]
                         [--ssh-key NAME_OR_ID ...] [--authorized-key PATH ...] [--yes]
  tools/do_vm.py status
  tools/do_vm.py sync
  tools/do_vm.py test    [-- <extra cargo args>]
  tools/do_vm.py ssh
  tools/do_vm.py down    [--yes]

Design notes: `docs/design/do-vm-harness.md`. This is a test harness, never a supported
deployment path.
"""

from __future__ import annotations

import argparse
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
# DigitalOcean vendor list price as of 2026-11-16 — re-verify before relying on the cost line.
VOLUME_USD_PER_GIB_MONTH = 0.10
READY_TIMEOUT_SECONDS = 15 * 60
READY_POLL_SECONDS = 5
SSH_WAIT_TIMEOUT_SECONDS = 5 * 60
REMOTE_ROOT = "/root/slipfork"
SUITE = "critical_incus_live"
SUITE_PATH = PurePosixPath(f"crates/slipfork-engine/tests/{SUITE}.rs")
EXCLUDED_TOP_LEVEL = frozenset({"target", "site", ".worktrees"})
AUTHORIZED_KEYS = "/root/.ssh/authorized_keys"
SSH_AUTH_FAILURE = (
    "Permission denied (publickey",
    "Too many authentication failures",
)
SSH_HOST_KEY_FAILURE = (
    "REMOTE HOST IDENTIFICATION HAS CHANGED",
    "Host key verification failed",
)


class HarnessError(RuntimeError):
    """A fail-closed harness precondition, API call, or remote step failed."""


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


@dataclass(frozen=True, slots=True)
class Size:
    slug: str
    vcpus: int | None
    memory_mib: int | None
    disk_gib: int | None
    price_monthly: float | None


PROVISION_SCRIPT = """\
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
candidate="$(apt-cache policy incus | awk '/Candidate:/ {print $2}')"
if [ -z "$candidate" ] || [ "$candidate" = "(none)" ]; then
  echo 'slipfork: the `incus` package is unavailable in this image; apt-cache policy incus:' >&2
  apt-cache policy incus >&2
  exit 1
fi
if ! apt-get install -y --no-install-recommends zfsutils-linux incus; then
  echo 'slipfork: installing zfsutils-linux/incus failed; apt-cache policy incus:' >&2
  apt-cache policy incus >&2
  exit 1
fi
if zpool list -H -o name incuspool >/dev/null 2>&1; then
  echo 'slipfork: ZFS pool incuspool already exists'
else
  for _attempt in $(seq 1 30); do
    set -- /dev/disk/by-id/scsi-0DO_Volume_*
    if [ "$#" -eq 1 ] && [ -e "$1" ]; then
      break
    fi
    sleep 2
  done
  if [ "$#" -ne 1 ] || [ ! -e "$1" ]; then
    echo "slipfork: expected exactly one attached DO volume (/dev/disk/by-id/scsi-0DO_Volume_*), found: $*" >&2
    exit 1
  fi
  zpool create -o ashift=12 -O mountpoint=/var/lib/incus/disks/pool incuspool /dev/disk/by-id/scsi-0DO_Volume_*
fi
systemctl enable --now incus
for _attempt in $(seq 1 60); do
  if incus info >/dev/null 2>&1; then
    break
  fi
  sleep 2
done
if ! incus info >/dev/null 2>&1; then
  echo 'slipfork: incusd did not answer `incus info` within 120s' >&2
  journalctl -u incus --no-pager -n 80 >&2
  exit 1
fi
if incus storage show default >/dev/null 2>&1; then
  echo 'slipfork: Incus storage pool `default` already exists'
else
  incus storage create default zfs source=incuspool
fi
echo 'slipfork ready' > /root/incus-ready
"""


def cloud_init() -> str:
    """Wrap the idempotent provisioning script in a single cloud-init `runcmd` chain."""
    body = "\n".join(f"      {line}" for line in PROVISION_SCRIPT.splitlines())
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
    raw = item.get("volume_ids", [])
    if not isinstance(raw, list) or not all(isinstance(value, str) for value in raw):
        raise HarnessError(
            f"doctl droplet {item.get('id')} returned malformed volume_ids: {raw!r}"
        )
    return frozenset(raw)


def repo_root() -> Path:
    try:
        return guard.repository_root(Path.cwd())
    except guard.GuardError as error:
        raise HarnessError(str(error)) from error


def known_hosts_for(root: Path, droplet: Droplet) -> Path:
    directory = root / ".slipfork" / "do-vm"
    directory.mkdir(parents=True, exist_ok=True)
    return directory / f"known_hosts-{droplet.id}"


def ssh_argv(
    droplet: Droplet, known_hosts: Path, remote: str | None, *, batch: bool = True
) -> list[str]:
    # `remote` is a shell string sent as one SSH argv operand. It is safe without quoting because
    # every interpolated operand in this harness is a module constant (REMOTE_ROOT, AUTHORIZED_KEYS,
    # SUITE_PATH) or a `shlex.quote`-escaped extra; no unescaped user input reaches it.
    argv = [
        "ssh",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        f"UserKnownHostsFile={known_hosts}",
    ]
    if batch:
        argv.extend(["-o", "BatchMode=yes"])
    argv.append(f"root@{droplet.ip}")
    if remote is not None:
        argv.append(remote)
    return argv


def _confirm(question: str, assume_yes: bool) -> None:
    if assume_yes:
        return
    if not sys.stdin.isatty():
        raise HarnessError(
            f"{question}: refusing without confirmation in a non-interactive shell; re-run with --yes"
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
        raise HarnessError("no droplet tagged slipfork-dev-vm; run `tools/do_vm.py up`")
    return droplet


def _resolve_ssh_keys(doctl: Doctl, overrides: list[str]) -> list[str]:
    """Resolve ids embedded at create time: every visible key, or the requested subset."""
    keys = doctl.ssh_keys()
    if not keys:
        raise HarnessError(
            "the current doctl context reports no SSH keys; add one with "
            "`doctl compute ssh-key import` before `up`"
        )
    available = ", ".join(f"{key.get('name')} (id={key.get('id')})" for key in keys)
    if not overrides:
        return [str(key["id"]) for key in keys]
    selected: list[str] = []
    for override in overrides:
        matches = [
            key
            for key in keys
            if override in {str(key.get("id")), str(key.get("name"))}
        ]
        if not matches:
            raise HarnessError(
                f"--ssh-key '{override}' matches no key visible to this doctl context; "
                f"available: {available}"
            )
        if len(matches) > 1:
            raise HarnessError(
                f"--ssh-key '{override}' is ambiguous; use a key id: {available}"
            )
        key_id = str(matches[0]["id"])
        if key_id not in selected:
            selected.append(key_id)
    return selected


def _read_authorized_keys(paths: list[str]) -> str:
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
    return "".join(chunks)


def _install_authorized_keys(content: str, droplet: Droplet, known_hosts: Path) -> None:
    remote = (
        "umask 077 && install -d -m 700 /root/.ssh && "
        f"touch {AUTHORIZED_KEYS} && chmod 600 {AUTHORIZED_KEYS} && "
        f"cat >> {AUTHORIZED_KEYS} && "
        f"sort -u -o {AUTHORIZED_KEYS} {AUTHORIZED_KEYS}"
    )
    _run(ssh_argv(droplet, known_hosts, remote), input_text=content)


def _print_fingerprints(droplet: Droplet, known_hosts: Path) -> None:
    result = _run(
        ssh_argv(droplet, known_hosts, f"ssh-keygen -lf {AUTHORIZED_KEYS}"),
        check=False,
    )
    if result.returncode != 0:
        detail = _detail(result)
        raise HarnessError(
            f"could not read authorized-key fingerprints from {droplet.ip}: {detail}"
        )
    print("keys     authorized_keys fingerprints:")
    for line in result.stdout.splitlines():
        print(f"         {line}")


def _warn_missing_authorized_keys() -> None:
    print(
        "warning: no --authorized-key given; the droplet trusts only keys visible to the "
        "current doctl context, so a key registered on the account but omitted from "
        "`doctl compute ssh-key list` will not be able to log in",
        file=sys.stderr,
    )


def _cost_line(size: Size, region: str, volume_gb: int) -> str:
    volume_cost = VOLUME_USD_PER_GIB_MONTH * volume_gb
    volume = f"${volume_cost:.2f}/month volume ({volume_gb} GiB)"
    if size.price_monthly is None:
        return f"droplet {size.slug} ({region}): price not reported by doctl; {volume}"
    droplet = f"${size.price_monthly:.2f}/month droplet ({size.slug}, {region})"
    total = f"${size.price_monthly + volume_cost:.2f}/month"
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
            "Slipfork dev VM ZFS pool (disposable; tools/do_vm.py down --yes)",
            "--tag",
            TAG,
        ]
    )
    volumes = doctl.tagged_volumes()
    return _exactly_one("volume", volumes)


def _create_droplet(
    doctl: Doctl, region: str, size: str, ssh_keys: list[str], volume: Volume
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
            ",".join(ssh_keys),
            "--tag-name",
            TAG,
            "--volumes",
            volume.id,
            "--user-data",
            cloud_init(),
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


def _ready_probe(
    droplet: Droplet, known_hosts: Path
) -> subprocess.CompletedProcess[str]:
    return _run(
        ssh_argv(droplet, known_hosts, "test -f /root/incus-ready"), check=False
    )


def _ssh_failure(
    probe: subprocess.CompletedProcess[str], droplet: Droplet, known_hosts: Path
) -> None:
    detail = _detail(probe)
    if any(marker in detail for marker in SSH_AUTH_FAILURE):
        raise HarnessError(
            f"SSH authentication to {droplet.ip} was rejected: {detail}; fix the key "
            "embedded at create time with `--ssh-key`, or install your key after boot "
            "with `--authorized-key <path>`"
        )
    if any(marker in detail for marker in SSH_HOST_KEY_FAILURE):
        raise HarnessError(
            f"SSH host-key check failed for {droplet.ip}; delete {known_hosts} only if the "
            "droplet was recreated at the same address"
        )


def _cloud_init_running(droplet: Droplet, known_hosts: Path) -> bool:
    probe = _run(ssh_argv(droplet, known_hosts, "cloud-init status"), check=False)
    return probe.returncode == 0 and "status: running" in (probe.stdout or "")


def _provision(droplet: Droplet, known_hosts: Path) -> None:
    probe = _run(
        ssh_argv(droplet, known_hosts, "bash -s"),
        input_text=PROVISION_SCRIPT,
        check=False,
    )
    if probe.returncode != 0:
        _ssh_failure(probe, droplet, known_hosts)
        raise HarnessError(
            f"provisioning {droplet.ip} failed ({probe.returncode}): {_detail(probe)}"
        )


def _poll(
    probe: Callable[[], subprocess.CompletedProcess[str]],
    *,
    droplet: Droplet,
    known_hosts: Path,
    timeout_seconds: int,
) -> str | None:
    """Poll until ready (`None`) or timeout (`last probe detail`).

    Only `_ssh_failure` (auth/host-key) may abort early. Every other SSH failure is treated as
    not-yet-up and keeps polling until the caller's bounded timeout.
    """
    deadline = time.monotonic() + timeout_seconds
    last_detail = ""
    while time.monotonic() < deadline:
        result = probe()
        if result.returncode == 0:
            return None
        _ssh_failure(result, droplet, known_hosts)
        last_detail = _detail(result)
        time.sleep(READY_POLL_SECONDS)
    return last_detail


def _wait_ssh(droplet: Droplet, known_hosts: Path) -> None:
    detail = _poll(
        lambda: _run(ssh_argv(droplet, known_hosts, "true"), check=False),
        droplet=droplet,
        known_hosts=known_hosts,
        timeout_seconds=SSH_WAIT_TIMEOUT_SECONDS,
    )
    if detail is None:
        return
    raise HarnessError(
        f"SSH did not become reachable at {droplet.ip} within "
        f"{SSH_WAIT_TIMEOUT_SECONDS // 60} minutes; last probe: {detail or 'no stderr'}"
    )


def _wait_ready(root: Path, droplet: Droplet) -> None:
    known_hosts = known_hosts_for(root, droplet)
    detail = _poll(
        lambda: _ready_probe(droplet, known_hosts),
        droplet=droplet,
        known_hosts=known_hosts,
        timeout_seconds=READY_TIMEOUT_SECONDS,
    )
    if detail is None:
        return

    print(
        f"error: cloud-init did not write /root/incus-ready within "
        f"{READY_TIMEOUT_SECONDS // 60} minutes; last cloud-init output:",
        file=sys.stderr,
    )
    for remote in (
        "tail -n 100 /var/log/cloud-init-output.log",
        "journalctl -u cloud-init --no-pager -n 80",
    ):
        logs = _run(ssh_argv(droplet, known_hosts, remote), check=False)
        print(f"$ ssh root@{droplet.ip} {remote}", file=sys.stderr)
        print((logs.stdout or "") + (logs.stderr or ""), file=sys.stderr)
    raise HarnessError(
        f"cloud-init provisioning on {droplet.ip} did not finish; last probe: "
        f"{detail or 'no stderr'}; inspect the log above; "
        "run `tools/do_vm.py down --yes` to stop billing now"
    )


def _print_summary(droplet: Droplet, volume: Volume, cost: str) -> None:
    print(
        f"droplet  {droplet.name}  id={droplet.id}  region={droplet.region}  ip={droplet.ip}"
    )
    print(
        f"volume   {volume.name}  id={volume.id}  region={volume.region}  {volume.size_gb} GiB"
    )
    print(
        "ready    /root/incus-ready written (Incus system containers only, no nested KVM; "
        "ZFS pool `incuspool`, storage pool `default`)"
    )
    print(f"ssh      ssh root@{droplet.ip}")
    print(f"cost     {cost}")
    print(
        "prereq   install Rust >= 1.85 (rustup) on the VM before `tools/do_vm.py test`; "
        "cloud-init provisions ZFS + Incus only"
    )
    print("next     tools/do_vm.py sync && tools/do_vm.py test")
    print("down     tools/do_vm.py down --yes")


def cmd_up(args: argparse.Namespace) -> int:
    if args.volume_gb < 1:
        raise HarnessError("--volume-gb must be a positive integer")
    root = repo_root()
    authorized_keys = (
        _read_authorized_keys(args.authorized_key) if args.authorized_key else None
    )
    doctl = Doctl()
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

    requested_size = droplet.size if droplet is not None else args.size
    size = doctl.size(requested_size)
    if size is None and droplet is None:
        raise HarnessError(
            f"size '{args.size}' is not offered; run `doctl compute size list`"
        )
    if size is None:
        size = Size(
            slug=requested_size,
            vcpus=None,
            memory_mib=None,
            disk_gib=None,
            price_monthly=None,
        )
    volume_gb = volume.size_gb if volume is not None else args.volume_gb
    cost = _cost_line(size, args.region, volume_gb)

    if droplet is not None or volume is not None:
        print(
            "note: existing tagged droplet/volume win; --size and --volume-gb apply only "
            "to newly created resources"
        )

    need_droplet = droplet is None
    need_volume = volume is None
    already_attached = (
        droplet is not None and volume is not None and volume.id in droplet.volume_ids
    )
    need_attach = droplet is not None and volume is not None and not already_attached

    if need_droplet:
        key_ids = _resolve_ssh_keys(doctl, args.ssh_key)

    if not need_droplet and not need_volume and not need_attach:
        if droplet is None or volume is None:
            raise HarnessError("reuse branch lost its tagged droplet/volume")
        print(f"reusing droplet {droplet.id} ({droplet.ip}) and volume {volume.id}")
    elif not need_droplet and not need_volume:
        if droplet is None or volume is None:
            raise HarnessError("attach branch lost its tagged droplet/volume")
        print(
            f"reusing droplet {droplet.id} ({droplet.ip}) and volume {volume.id}; "
            "the volume is not attached"
        )
    else:
        print(f"region   {args.region}")
        print(
            f"size     {size.slug} ({size.vcpus or '?'} vCPU, {size.memory_mib or '?'} MiB RAM, "
            f"{size.disk_gib or '?'} GiB SSD)"
        )
        if need_volume:
            print(f"volume   create {args.volume_gb} GiB, tag {TAG}")
        else:
            if volume is None:
                raise HarnessError("volume reuse branch lost its tagged volume")
            print(f"volume   reuse {volume.id} ({volume.size_gb} GiB)")
        if need_droplet:
            print(f"image    {IMAGE_SLUG}")
            scope = (
                "restricted by --ssh-key"
                if args.ssh_key
                else "all keys visible to this doctl context"
            )
            print(f"keys     {', '.join(key_ids)} ({scope})")
        print(f"cost     {cost}")
        print("down     tools/do_vm.py down --yes")
        _confirm(
            f"create the missing tagged droplet/volume in {args.region} for this cost?",
            args.yes,
        )

    created_droplet = False
    created_volume = False
    attached_volume = False
    try:
        if need_volume:
            volume = _create_volume(doctl, args.region, args.volume_gb)
            created_volume = True
        if volume is None:
            raise HarnessError("no tagged volume available after create/reuse")
        if need_droplet:
            droplet = _create_droplet(doctl, args.region, args.size, key_ids, volume)
            created_droplet = True
        elif not already_attached:
            _attach_volume(droplet, volume)
            attached_volume = True
    except HarnessError as error:
        raise HarnessError(
            f"{error}\nnote: a partially created tagged resource may exist; "
            "run `tools/do_vm.py down --yes` to clean up"
        ) from error

    if droplet is None:
        raise HarnessError("no tagged droplet available after create/reuse")
    known_hosts = known_hosts_for(root, droplet)
    reprovision = not created_droplet and (created_volume or attached_volume)
    if (
        not created_droplet
        and not reprovision
        and (probe := _ready_probe(droplet, known_hosts)).returncode != 0
    ):
        _ssh_failure(probe, droplet, known_hosts)
        reprovision = not _cloud_init_running(droplet, known_hosts)
    if reprovision:
        _wait_ssh(droplet, known_hosts)
        _provision(droplet, known_hosts)
    _wait_ready(root, droplet)
    if authorized_keys is not None:
        try:
            _install_authorized_keys(authorized_keys, droplet, known_hosts)
        except HarnessError as error:
            raise HarnessError(
                f"{error}\nrun `tools/do_vm.py down --yes` to stop billing now"
            ) from error
    else:
        _warn_missing_authorized_keys()
    _print_summary(droplet, volume, cost)
    try:
        _print_fingerprints(droplet, known_hosts)
    except HarnessError as error:
        raise HarnessError(
            f"{error}\nrun `tools/do_vm.py down --yes` to stop billing now"
        ) from error
    return 0


def cmd_status(args: argparse.Namespace) -> int:
    root = repo_root()
    doctl = Doctl()
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
        _print_fingerprints(droplet, known_hosts_for(root, droplet))
    if len(droplets) == 1:
        print(f"ssh      ssh root@{droplets[0].ip}")
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
    process = subprocess.Popen(ssh, stdin=subprocess.PIPE, stderr=subprocess.PIPE)
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
            f"`ssh` tar-pipe into {REMOTE_ROOT} failed ({returncode}): {detail}"
        )


def cmd_sync(args: argparse.Namespace) -> int:
    root = repo_root()
    droplet = _single_droplet(Doctl())
    sync_paths = _pruned(_git_files(root, "--cached", "--others", "--exclude-standard"))
    _refuse_secrets(sync_paths)
    known_hosts = known_hosts_for(root, droplet)
    _run(ssh_argv(droplet, known_hosts, f"mkdir -p {REMOTE_ROOT}"))
    _stream_tar(
        root, sync_paths, ssh_argv(droplet, known_hosts, f"tar -x -C {REMOTE_ROOT}")
    )
    print(f"synced {len(sync_paths)} file(s) to {REMOTE_ROOT} on {droplet.ip}")
    return 0


def cmd_test(args: argparse.Namespace) -> int:
    root = repo_root()
    if not (root / SUITE_PATH).is_file():
        raise HarnessError(
            f"missing live Incus conformance suite {SUITE_PATH} (delivered by M3.3); "
            f"`{SUITE}` cannot run yet"
        )
    droplet = _single_droplet(Doctl())
    known_hosts = known_hosts_for(root, droplet)
    remote_suite = f"{REMOTE_ROOT}/{SUITE_PATH}"
    probe = _run(ssh_argv(droplet, known_hosts, f"test -f {remote_suite}"), check=False)
    if probe.returncode != 0:
        if probe.returncode == 255:
            _ssh_failure(probe, droplet, known_hosts)
            raise HarnessError(f"cannot reach {droplet.ip} over SSH: {_detail(probe)}")
        raise HarnessError(
            f"missing live Incus conformance suite {remote_suite} on the VM (M3.3); "
            "run `tools/do_vm.py sync` first"
        )
    cargo = _run(ssh_argv(droplet, known_hosts, "command -v cargo"), check=False)
    if cargo.returncode != 0:
        raise HarnessError(
            "cargo is not installed on the VM; this harness provisions ZFS + Incus only — "
            "install Rust >= 1.85 (edition 2024) there before `test`"
        )
    command = (
        f"cd {REMOTE_ROOT} && cargo test -p slipfork-engine --test {SUITE} "
        "-- --test-threads=1"
    )
    extras = [arg for arg in args.cargo_args if arg != "--"]
    if extras:
        command += " " + " ".join(shlex.quote(arg) for arg in extras)
    return _run(
        ssh_argv(droplet, known_hosts, command), capture=False, check=False
    ).returncode


def cmd_ssh(args: argparse.Namespace) -> int:
    root = repo_root()
    droplet = _single_droplet(Doctl())
    argv = ssh_argv(droplet, known_hosts_for(root, droplet), None, batch=False)
    try:
        os.execvp("ssh", argv)
    except OSError as error:
        raise HarnessError(f"could not exec ssh: {error}") from error
    return 0


def cmd_down(args: argparse.Namespace) -> int:
    root = repo_root()
    doctl = Doctl()
    droplets = doctl.tagged_droplets()
    volumes = doctl.tagged_volumes()
    if not droplets and not volumes:
        print(f"nothing to destroy: no resources tagged {TAG}")
        return 0
    print(
        f"destroying {len(droplets)} droplet(s) and {len(volumes)} volume(s) tagged {TAG}:"
    )
    for droplet in droplets:
        print(f"  droplet {droplet.name} id={droplet.id} ip={droplet.ip}")
    for volume in volumes:
        print(f"  volume  {volume.name} id={volume.id} {volume.size_gb} GiB")
    _confirm("destroy these tagged resources permanently?", args.yes)
    for droplet in droplets:
        _run(["doctl", "compute", "droplet", "delete", str(droplet.id), "--force"])
        known_hosts_for(root, droplet).unlink(missing_ok=True)
    for volume in volumes:
        _run(["doctl", "compute", "volume", "delete", volume.id, "--force"])
    print(f"destroyed {len(droplets)} droplet(s) and {len(volumes)} volume(s)")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="tools/do_vm.py",
        description="DigitalOcean dev-VM harness for live IncusEngine tests.",
        epilog="See docs/design/do-vm-harness.md.",
    )
    commands = parser.add_subparsers(dest="command", required=True, metavar="command")

    up = commands.add_parser(
        "up", help="create the tagged droplet + ZFS volume and wait for Incus"
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
    up.add_argument(
        "--ssh-key",
        action="append",
        default=[],
        metavar="NAME_OR_ID",
        help="restrict the droplet to this visible key (repeatable); default: every visible key",
    )
    up.add_argument(
        "--authorized-key",
        action="append",
        default=[],
        metavar="PATH",
        help=(
            "append this public-key file to /root/.ssh/authorized_keys after cloud-init "
            "(repeatable; idempotent via sort -u)"
        ),
    )
    up.add_argument(
        "--yes", action="store_true", help="skip the cost confirmation prompt"
    )
    up.set_defaults(handler=cmd_up)

    status = commands.add_parser(
        "status", help="report tagged droplet and volume state"
    )
    status.set_defaults(handler=cmd_status)

    sync = commands.add_parser(
        "sync", help="tar-pipe the working tree to /root/slipfork"
    )
    sync.set_defaults(handler=cmd_sync)

    test = commands.add_parser(
        "test", help="stream the live IncusEngine conformance suite"
    )
    test.add_argument(
        "cargo_args",
        nargs=argparse.REMAINDER,
        metavar="-- <cargo args>",
        help="extra arguments forwarded to `cargo test` after `--test-threads=1`",
    )
    test.set_defaults(handler=cmd_test)

    ssh = commands.add_parser("ssh", help="open an interactive root SSH session")
    ssh.set_defaults(handler=cmd_ssh)

    down = commands.add_parser("down", help="destroy the tagged droplet and volume")
    down.add_argument(
        "--yes", action="store_true", help="skip the destructive confirmation prompt"
    )
    down.set_defaults(handler=cmd_down)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return int(args.handler(args))
    except HarnessError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
