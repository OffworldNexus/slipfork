"""Unit tests for `tools/do_vm.py` driven by fake `doctl`/`ssh` executables on `PATH`.

No test touches the DigitalOcean API or a real VM: every cloud/SSH call lands in a script fake
that logs argv and keeps its state under `tmp_path`. The tool runs in-process so the bounded
waits can be patched; the bash renderers (`render_volume_discovery`,
`render_volume_signature_step`, `render_automount_teardown`) run for real under `bash` with fake
executables on `PATH`.
"""

from __future__ import annotations

import contextlib
import getpass
import importlib.util
import io
import json
import os
import re
import shlex
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from unittest import mock

import tempfile
import unittest

TOOLS_DIR = Path(__file__).resolve().parent.parent
REPO_ROOT = TOOLS_DIR.parent
TOOL = TOOLS_DIR / "do_vm.py"

sys.path.insert(0, str(TOOLS_DIR))
_SPEC = importlib.util.spec_from_file_location("do_vm_under_test", TOOL)
if _SPEC is None or _SPEC.loader is None:
    raise RuntimeError("cannot load tools/do_vm.py for script tests")
do_vm_mod = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = do_vm_mod
_SPEC.loader.exec_module(do_vm_mod)

FAKE_DOCTL = (
    "#!"
    + sys.executable
    + """
import json
import os
import sys
from pathlib import Path

state = Path(os.environ["FAKE_STATE_DIR"])
with Path(os.environ["FAKE_DOCTL_LOG"]).open("a", encoding="utf-8") as log:
    log.write(" ".join(sys.argv[1:]).replace(chr(10), " ") + "\\n")

argv = sys.argv[1:]
if argv[-2:] == ["--output", "json"]:
    argv = argv[:-2]


def load(name, default):
    path = state / name
    if not path.is_file():
        return default
    return json.loads(path.read_text(encoding="utf-8"))


def save(name, value):
    (state / name).write_text(json.dumps(value), encoding="utf-8")


def value_after(flag):
    return argv[argv.index(flag) + 1]


if argv[:3] == ["compute", "ssh-key", "list"]:
    print(json.dumps(load("ssh-keys.json", [])))
elif argv[:3] == ["compute", "ssh-key", "import"]:
    if os.environ.get("FAKE_IMPORT_FAIL"):
        print(os.environ["FAKE_IMPORT_FAIL"], file=sys.stderr)
        sys.exit(1)
    keys = load("ssh-keys.json", [])
    body = Path(value_after("--public-key-file")).read_text(encoding="utf-8").strip()
    if os.environ.get("FAKE_IMPORT_INVISIBLE"):
        print(json.dumps({"id": 999, "name": argv[3], "public_key": body}))
        sys.exit(0)
    keys.append({"id": 900 + len(keys), "name": argv[3], "public_key": body})
    save("ssh-keys.json", keys)
    print(json.dumps(keys[-1]))
elif argv[:3] == ["compute", "region", "list"]:
    print(json.dumps([{"slug": "ams3", "available": True}, {"slug": "lon1", "available": True}]))
elif argv[:3] == ["compute", "size", "list"]:
    price = None if os.environ.get("FAKE_SIZE_PRICE_NULL") else 48.0
    print(json.dumps([{"slug": "s-4vcpu-8gb", "vcpus": 4, "memory": 8192, "disk": 160,
                       "price_monthly": price, "regions": ["ams3", "lon1"]}]))
elif argv[:3] == ["compute", "droplet", "list"]:
    print(json.dumps(load("droplet.json", [])))
elif argv[:3] == ["compute", "volume", "list"]:
    print(json.dumps(load("volume.json", [])))
elif argv[:3] == ["compute", "volume", "create"]:
    save("volume.json", [{
        "id": "vol-123",
        "name": "slipfork-dev-vm",
        "region": {"slug": value_after("--region")},
        "size_gigabytes": int(value_after("--size").removesuffix("GiB")),
        "tags": ["slipfork-dev-vm"],
        "droplet_ids": [],
    }])
    print("{}")
elif argv[:3] == ["compute", "droplet", "create"]:
    (state / "user-data.txt").write_text(value_after("--user-data"), encoding="utf-8")
    save("droplet.json", [{
        "id": 9001,
        "name": "slipfork-dev-vm",
        "region": {"slug": value_after("--region")},
        "size": {"slug": value_after("--size")},
        "volume_ids": [value_after("--volumes")],
        "networks": {"v4": [{"type": "public", "ip_address": "203.0.113.10"}]},
    }])
    (state / "created").write_text("1", encoding="utf-8")
    print("{}")
elif argv[:3] == ["compute", "volume-action", "attach"]:
    droplet = load("droplet.json", [])
    volume = load("volume.json", [])
    for item in droplet:
        item["volume_ids"] = [argv[3]]
    for item in volume:
        item["droplet_ids"] = [int(argv[4])]
    save("droplet.json", droplet)
    save("volume.json", volume)
    print("{}")
elif argv[:3] == ["compute", "volume-action", "detach"]:
    droplet = load("droplet.json", [])
    volume = load("volume.json", [])
    for item in droplet:
        item["volume_ids"] = []
    for item in volume:
        item["droplet_ids"] = []
    save("droplet.json", droplet)
    save("volume.json", volume)
    print("{}")
elif argv[:3] == ["compute", "droplet", "delete"]:
    if not os.environ.get("FAKE_DELETE_STUCK"):
        (state / "droplet.json").unlink(missing_ok=True)
        (state / "created").unlink(missing_ok=True)
    print("{}")
elif argv[:3] == ["compute", "volume", "delete"]:
    (state / "volume.json").unlink(missing_ok=True)
    print("{}")
else:
    print("fake doctl: unhandled argv: " + " ".join(argv), file=sys.stderr)
    sys.exit(9)
"""
)

FAKE_SSH = (
    "#!"
    + sys.executable
    + """
import io
import json
import os
import sys
import tarfile
from pathlib import Path

state = Path(os.environ["FAKE_STATE_DIR"])
with Path(os.environ["FAKE_SSH_LOG"]).open("a", encoding="utf-8") as log:
    log.write(" ".join(sys.argv[1:]).replace(chr(10), " ") + "\\n")

argv = sys.argv[1:]
remote = argv[-1] if argv else ""


def env_int(name, default=0):
    return int(os.environ.get(name, str(default)))


def env(name, default=""):
    return os.environ.get(name, default)


if "for _file in /root/.ssh/authorized_keys" in remote:
    if env_int("FAKE_FINGERPRINT_EXIT") != 0:
        print("no such file", file=sys.stderr)
        sys.exit(env_int("FAKE_FINGERPRINT_EXIT"))
    print("== /root/.ssh/authorized_keys")
    print("256 SHA256:FAKEKEY slipfork-do-vm (ED25519)")
    print("== /home/slipfork/.ssh/authorized_keys")
    print("256 SHA256:FAKEKEY slipfork-do-vm (ED25519)")
    sys.exit(0)
if remote.startswith("test -f /var/lib/slipfork/.slipfork-ready"):
    if (state / "created").is_file():
        sys.exit(0)
    if env("FAKE_READY_STDERR"):
        print(env("FAKE_READY_STDERR"), file=sys.stderr)
    sys.exit(env_int("FAKE_READY_EXIT"))
if remote == "cloud-init status":
    print("status: running" if env("FAKE_CLOUD_INIT_RUNNING") else "status: done")
    sys.exit(0)
if remote.startswith("test -f /home/slipfork/slipfork/crates/"):
    sys.exit(env_int("FAKE_REMOTE_SUITE_EXIT"))
if remote.endswith("cargo --version"):
    if env_int("FAKE_CARGO_EXIT") != 0:
        sys.exit(env_int("FAKE_CARGO_EXIT"))
    print(env("FAKE_CARGO_VERSION", "cargo 1.94.0 (0000000 2026-01-01)"))
    sys.exit(0)
if remote.startswith("bash -s ") and remote != "bash -s":
    label = remote.split(" ", 2)[2]
    (state / f"{label}.sh").write_text(sys.stdin.read(), encoding="utf-8")
    env_label = label.upper().replace("-", "_")
    if env(f"FAKE_{env_label}_STDERR"):
        print(env(f"FAKE_{env_label}_STDERR"), file=sys.stderr)
    sys.exit(env_int(f"FAKE_{env_label}_EXIT"))
if remote == "bash -s":
    (state / "provision.txt").write_text(sys.stdin.read(), encoding="utf-8")
    if env("FAKE_PROVISION_STDERR"):
        print(env("FAKE_PROVISION_STDERR"), file=sys.stderr)
    sys.exit(env_int("FAKE_PROVISION_EXIT"))
if remote.startswith("bash -c ") and "_key_blob" in remote:
    blob = sys.stdin.read()
    (state / "install-keys.sh").write_text(remote, encoding="utf-8")
    for name in ("root_authorized_keys", "slipfork_authorized_keys"):
        path = state / name
        existing = path.read_text(encoding="utf-8") if path.is_file() else ""
        merged = sorted({line for line in (existing + blob).splitlines() if line})
        path.write_text("".join(f"{line}\\n" for line in merged), encoding="utf-8")
    sys.exit(0)
if remote.startswith("tar -x -C "):
    data = sys.stdin.buffer.read()
    destination = state / "remote"
    destination.mkdir(parents=True, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        archive.extractall(destination, filter="data")
    sys.exit(0)
if "command -v docker" in remote:
    sys.exit(0 if env("FAKE_DOCKER_PRESENT") else 1)
if "cargo metadata --format-version" in remote:
    if env("FAKE_METADATA_JSON"):
        print(env("FAKE_METADATA_JSON"))
    else:
        print(json.dumps({"packages": [
            {"name": "slipfork", "targets": [
                {"name": "slipfork", "kind": ["bin"]},
                {"name": "slipfork", "kind": ["test"]},
            ]},
            {"name": "slipfork-world", "targets": [
                {"name": "slipfork-world-init", "kind": ["bin"]},
            ]},
            {"name": "slipfork-ingress", "targets": [
                {"name": "unrelated", "kind": ["bin"]},
            ]},
        ]}))
    sys.exit(env_int("FAKE_METADATA_EXIT"))
if "cargo build" in remote:
    with (state / "builds.log").open("a", encoding="utf-8") as log:
        log.write(remote + "\\n")
    sys.exit(env_int("FAKE_BUILD_EXIT"))
if "cargo test -p " in remote:
    with (state / "suites.log").open("a", encoding="utf-8") as log:
        log.write(remote + "\\n")
    if "slipfork-world" in remote:
        sys.exit(env_int("FAKE_WORLD_EXIT"))
    sys.exit(env_int("FAKE_CLI_EXIT"))
sys.exit(0)
"""
)

PUBLIC_KEY = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFAKETESTKEY owner-laptop\n"
OWNER_KEY = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQownerkey owner@example\n"


@dataclass
class Result:
    code: int
    stdout: str
    stderr: str


@dataclass
class Harness:
    root: Path
    home: Path
    state: Path
    doctl_log: Path
    ssh_log: Path

    def run(self, *args: str, env: dict[str, str] | None = None) -> Result:
        out, err = io.StringIO(), io.StringIO()
        with (
            mock.patch.dict(os.environ, env or {}),
            contextlib.redirect_stdout(out),
            contextlib.redirect_stderr(err),
        ):
            try:
                code = do_vm_mod.main(list(args))
            except SystemExit as exit_error:
                code = int(exit_error.code or 0)
        return Result(code, out.getvalue(), err.getvalue())

    def doctl_calls(self) -> list[str]:
        return _lines(self.doctl_log)

    def ssh_calls(self) -> list[str]:
        return _lines(self.ssh_log)

    def write_state(self, name: str, value: object) -> None:
        (self.state / name).write_text(json.dumps(value), encoding="utf-8")

    def existing(self) -> None:
        """State for one tagged droplet attached to one tagged volume."""
        self.write_state("droplet.json", _droplet_state())
        self.write_state("volume.json", _volume_state())

    def key_file(self, name: str = "owner.pub", content: str = OWNER_KEY) -> Path:
        path = self.root / name
        path.write_text(content, encoding="utf-8")
        return path

    def materialize_identity(self) -> Path:
        return do_vm_mod.load_identity(None, materialize=True).path

    def suite_files(self) -> None:
        for _, _, relative, _ in do_vm_mod.SUITES:
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("#[test]\nfn live() {}\n", encoding="utf-8")


def _lines(path: Path) -> list[str]:
    if not path.is_file():
        return []
    return [line for line in path.read_text(encoding="utf-8").splitlines() if line]


def _write_executable(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(0o755)


def run_bash(
    script: str, bin_dir: Path, **env: str
) -> subprocess.CompletedProcess[str]:
    """Run a rendered snippet under real `bash` with `bin_dir` first on `PATH`."""
    bin_dir.mkdir(parents=True, exist_ok=True)
    return subprocess.run(
        ["bash", "-c", script],
        text=True,
        capture_output=True,
        check=False,
        env={
            **os.environ,
            "PATH": f"{bin_dir}:{os.environ['PATH']}",
            **env,
        },
    )


def _git_init(root: Path) -> None:
    env = {**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null"}
    subprocess.run(["git", "init", "-b", "develop"], cwd=root, check=True, env=env)
    for key, value in (("user.email", "test@example.com"), ("user.name", "Test")):
        subprocess.run(["git", "config", key, value], cwd=root, check=True, env=env)


def _droplet_state() -> list[dict[str, object]]:
    return [
        {
            "id": 9001,
            "name": "slipfork-dev-vm",
            "region": {"slug": "ams3"},
            "size": {"slug": "s-4vcpu-8gb"},
            "volume_ids": ["vol-123"],
            "networks": {"v4": [{"type": "public", "ip_address": "203.0.113.10"}]},
        }
    ]


def _volume_state() -> list[dict[str, object]]:
    return [
        {
            "id": "vol-123",
            "name": "slipfork-dev-vm",
            "region": {"slug": "ams3"},
            "size_gigabytes": 100,
            "tags": ["slipfork-dev-vm"],
            "droplet_ids": [9001],
        }
    ]


def _only(calls: list[str], needle: str) -> str:
    matches = [call for call in calls if needle in call]
    assert len(matches) == 1, f"expected exactly one {needle!r} call, got {matches}"
    return matches[0]


def _deletions(calls: list[str]) -> list[str]:
    return [
        call
        for call in calls
        if "compute droplet delete" in call or "compute volume delete" in call
    ]


def _signature_bin(tmp_path: Path) -> Path:
    bin_dir = tmp_path / "fakebin"
    bin_dir.mkdir(exist_ok=True)
    _write_executable(
        bin_dir / "blkid",
        """#!/bin/sh
case "$*" in
  *"-s TYPE"*)
    printf '%s' "${FAKE_BLKID_TYPE:-}"
    printf '%s' "${FAKE_BLKID_ERR:-}" >&2
    exit "${FAKE_BLKID_RC:-2}" ;;
  *"-s UUID"*)
    printf '%s' "${FAKE_BLKID_UUID:-uuid-1}"
    exit "${FAKE_BLKID_UUID_RC:-0}" ;;
esac
exit 9
""",
    )
    _write_executable(
        bin_dir / "dd",
        '#!/bin/sh\nprintf \'%s\' "${FAKE_DD_STDERR:-}" >&2\nexit "${FAKE_DD_RC:-0}"\n',
    )
    _write_executable(
        bin_dir / "wipefs",
        '#!/bin/sh\nprintf \'%s\' "${FAKE_WIPEFS_OUT:-}"\nexit "${FAKE_WIPEFS_RC:-0}"\n',
    )
    _write_executable(
        bin_dir / "mkfs.btrfs",
        '#!/bin/sh\nprintf \'%s\\n\' "$*" >> "$FAKE_MKFS_LOG"\nexit 0\n',
    )
    return bin_dir


def _run_signature_step(
    tmp_path: Path,
    device: Path,
    env: dict[str, str],
    *,
    allow_format: bool,
) -> subprocess.CompletedProcess[str]:
    config = do_vm_mod.ProvisionConfig(
        allow_format=allow_format,
        device_wait_attempts=1,
        device_wait_seconds=0,
    )
    script = (
        f"volume_device={shlex.quote(str(device))}\n"
        + do_vm_mod.render_volume_signature_step(config)
    )
    return run_bash(
        script,
        _signature_bin(tmp_path),
        FAKE_MKFS_LOG=str(tmp_path / "mkfs.log"),
        **env,
    )


def _mkfs_calls(tmp_path: Path) -> list[str]:
    return _lines(tmp_path / "mkfs.log")


MIB = 1024 * 1024
ZERO_8MIB = b"\0" * (8 * MIB)
NONZERO_IN_MIB_1_TO_8 = b"\0" * MIB + b"\1" + b"\0" * (7 * MIB - 1)


def _demo_config(tmp_path: Path, **overrides: str) -> object:
    """A `ProvisionConfig` whose tree, demo, and fake home all live under `tmp_path`."""
    tree = tmp_path / "tree"
    template = tree / do_vm_mod.DEMO_TEMPLATE
    template.mkdir(parents=True)
    for name, body in {
        "snow.yml": "images: []\n",
        "Dockerfile": "FROM scratch AS app\n",
        "README.md": "the tracked demo\n",
        "slipfork.toml": 'project = "demo"\n',
    }.items():
        (template / name).write_text(body, encoding="utf-8")
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    values: dict[str, str] = {
        "remote_tree": str(tree),
        "remote_demo": str(tmp_path / "demo"),
        "remote_home": str(home),
        "remote_user": getpass.getuser(),
    }
    values.update(overrides)
    return do_vm_mod.ProvisionConfig(**values)


FAKE_RUNUSER = """#!/bin/sh
# runuser -u <user> -- <command...>
shift 2
[ "$1" = "--" ] && shift
exec "$@"
"""

FAKE_SLIPFORK = (
    "#!"
    + sys.executable
    + """
import os
import sys
from pathlib import Path

with Path(os.environ["FAKE_COMMAND_LOG"]).open("a", encoding="utf-8") as log:
    log.write("slipfork " + " ".join(sys.argv[1:]) + "\\n")
argv = sys.argv[1:]
if argv == ["daemon", "install", "--help"]:
    if os.environ.get("FAKE_SLIPFORK_NO_DAEMON"):
        print("error: unknown argument 'daemon'", file=sys.stderr)
        sys.exit(2)
    sys.exit(int(os.environ.get("FAKE_SLIPFORK_HELP_EXIT", "0")))
sys.exit(int(os.environ.get("FAKE_SLIPFORK_EXIT", "0")))
"""
)

FAKE_INSTALL = (
    "#!"
    + sys.executable
    + """
import os
import shutil
import sys
from pathlib import Path

args = sys.argv[1:]
if "-d" in args:
    os.execv("/usr/bin/install", ["install", *args])
with Path(os.environ["FAKE_COMMAND_LOG"]).open("a", encoding="utf-8") as log:
    log.write("install " + " ".join(args) + "\\n")
shutil.copyfile(args[-2], args[-1])
os.chmod(args[-1], 0o755)
"""
)

FAKE_SYSTEMCTL = """#!/bin/sh
echo "systemctl $*" >> "$FAKE_COMMAND_LOG"
if [ "$1" = "show" ]; then
  for _arg in "$@"; do
    _unit="$_arg"
  done
  if [ -n "${FAKE_UNIT_STATE_DIR:-}" ] && [ -f "${FAKE_UNIT_STATE_DIR}/${_unit}.state" ]; then
    cat "${FAKE_UNIT_STATE_DIR}/${_unit}.state"
  else
    echo "not-found"
  fi
  exit 0
fi
case "$*" in
  *enable*|*disable*|*daemon-reload*) exit 0 ;;
esac
echo "${FAKE_SYSTEMCTL_STATE:-active}"
if [ -n "${FAKE_SYSTEMCTL_STDERR:-}" ]; then
  printf '%s\\n' "$FAKE_SYSTEMCTL_STDERR" >&2
fi
case "${FAKE_SYSTEMCTL_STATE:-active}" in
  active) exit 0 ;;
  *) exit 3 ;;
esac
"""

FAKE_LOGINCTL = """#!/bin/sh
echo "loginctl $*" >> "$FAKE_COMMAND_LOG"
"""

FAKE_AA_STATUS = """#!/bin/sh
echo "aa-status" >> "$FAKE_COMMAND_LOG"
if [ -n "${FAKE_AA_STATUS_FAIL:-}" ]; then
  echo "apparmor is not loaded" >&2
  exit 1
fi
printf 'profiles are in enforce mode.\\n   slipfork\\n'
"""

FAKE_ID = """#!/bin/sh
echo "id $*" >> "$FAKE_COMMAND_LOG"
echo 1000
"""


def _fake_bin(tmp_path: Path) -> Path:
    """The one fake privileged-verb `PATH` prefix every rendered snippet runs against."""
    bin_dir = tmp_path / "fakebin"
    bin_dir.mkdir(exist_ok=True)
    for name, body in {
        "install": FAKE_INSTALL,
        "slipfork": FAKE_SLIPFORK,
        "systemctl": FAKE_SYSTEMCTL,
        "aa-status": FAKE_AA_STATUS,
        "id": FAKE_ID,
        "loginctl": FAKE_LOGINCTL,
        "runuser": FAKE_RUNUSER,
    }.items():
        _write_executable(bin_dir / name, body)
    return bin_dir


def _product_binaries_tree(config: object) -> Path:
    """Create the built binaries under the configured `target/<profile>`."""
    sources = Path(config.remote_tree) / "target" / do_vm_mod.BUILD_PROFILE
    sources.mkdir(parents=True)
    for binary in do_vm_mod.PRODUCT_BINARIES:
        _write_executable(sources / binary, "#!/bin/sh\nexit 0\n")
    return sources


def _run_step(
    script: str, bin_dir: Path, tmp_path: Path, **env: str
) -> subprocess.CompletedProcess[str]:
    return run_bash(
        script,
        bin_dir,
        FAKE_COMMAND_LOG=str(tmp_path / "commands.log"),
        PATH=f"{bin_dir}:{os.environ['PATH']}",
        **env,
    )


def _git_demo(demo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(demo), *args],
        capture_output=True,
        text=True,
        check=True,
        env={**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null"},
    ).stdout


class _MonkeyPatch:
    """Minimal stdlib stand-in for pytest's monkeypatch fixture."""

    def __init__(self) -> None:
        self._undo: list = []

    def setattr(self, target: object, name: str, value: object) -> None:
        old = getattr(target, name)
        self._undo.append(lambda: setattr(target, name, old))
        setattr(target, name, value)

    def setenv(self, name: str, value: str) -> None:
        old = os.environ.get(name)
        self._undo.append(
            lambda: (
                os.environ.__setitem__(name, old)
                if old is not None
                else os.environ.pop(name, None)
            )
        )
        os.environ[name] = value

    def chdir(self, path: Path) -> None:
        old = os.getcwd()
        self._undo.append(lambda: os.chdir(old))
        os.chdir(path)

    def undo(self) -> None:
        while self._undo:
            self._undo.pop()()


class HarnessTests(unittest.TestCase):
    """Every do_vm behaviour against fake `doctl`/`ssh` executables on `PATH`."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp_path = Path(self._tmp.name)
        self.monkeypatch = _MonkeyPatch()
        self.addCleanup(self.monkeypatch.undo)
        tmp_path = self.tmp_path
        monkeypatch = self.monkeypatch

        root = tmp_path / "repo"
        root.mkdir()
        _git_init(root)
        home = tmp_path / "home"
        home.mkdir()
        bin_dir = tmp_path / "bin"
        bin_dir.mkdir()
        state = tmp_path / "state"
        state.mkdir()
        doctl_log = tmp_path / "doctl.log"
        ssh_log = tmp_path / "ssh.log"
        _write_executable(bin_dir / "doctl", FAKE_DOCTL)
        _write_executable(bin_dir / "ssh", FAKE_SSH)
        (state / "ssh-keys.json").write_text(
            json.dumps(
                [
                    {"id": 101, "name": "bench-key", "public_key": PUBLIC_KEY},
                    {"id": 202, "name": "owner-laptop", "public_key": OWNER_KEY},
                ]
            ),
            encoding="utf-8",
        )
        for name, value in {
            "PATH": f"{bin_dir}:{os.environ['PATH']}",
            "HOME": str(home),
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "FAKE_STATE_DIR": str(state),
            "FAKE_DOCTL_LOG": str(doctl_log),
            "FAKE_SSH_LOG": str(ssh_log),
        }.items():
            monkeypatch.setenv(name, value)
        monkeypatch.chdir(root)
        monkeypatch.setattr(sys, "stdin", io.StringIO(""))
        for name, value in {
            "READY_POLL_SECONDS": 0,
            "READY_TIMEOUT_SECONDS": 0,
            "SSH_WAIT_TIMEOUT_SECONDS": 0,
            "COLD_BOOT_GRACE_SECONDS": 0,
            "DETACH_TIMEOUT_SECONDS": 0,
            "DROPLET_DELETE_TIMEOUT_SECONDS": 0,
            "ACCOUNT_KEY_TIMEOUT_SECONDS": 0,
            "DOWN_POLL_TIMEOUT_SECONDS": 0,
        }.items():
            monkeypatch.setattr(do_vm_mod, name, value)
        self.harness = Harness(root, home, state, doctl_log, ssh_log)

    def test_help_lists_exactly_the_six_commands(self):
        harness = self.harness
        result = harness.run("--help")
        assert result.code == 0, result.stderr
        commands: set[str] = set()
        in_commands = False
        for line in result.stdout.splitlines():
            if line.strip() == "command":
                in_commands = True
                continue
            if in_commands:
                if not line.strip():
                    break
                commands.add(line.split()[0])
        assert commands == {"up", "status", "sync", "test", "ssh", "down"}
        assert harness.doctl_calls() == []

    def test_up_help_lists_identity_authorized_key_and_format_blank(self):
        harness = self.harness
        result = harness.run("up", "--help")
        assert result.code == 0, result.stderr
        for flag in (
            "--region",
            "--size",
            "--volume-gb",
            "--yes",
            "--identity",
            "--authorized-key",
            "--format-blank",
        ):
            assert flag in result.stdout
        assert "--ssh-key" not in result.stdout
        assert "additional SSH public key" in result.stdout
        assert "first listed" not in result.stdout
        assert "DESTROYS any data" in result.stdout
        assert "never overrides a detected foreign signature" in result.stdout

        status_help = harness.run("status", "--help")
        assert status_help.code == 0, status_help.stderr
        assert "first listed" in status_help.stdout

    def test_up_creates_volume_then_droplet_with_btrfs_provisioning(self):
        harness = self.harness
        result = harness.run("up", "--yes")
        assert result.code == 0, result.stderr
        calls = harness.doctl_calls()

        volume_create = _only(calls, "compute volume create")
        assert "--region ams3" in volume_create
        assert "--size 100GiB" in volume_create
        assert "--tag slipfork-dev-vm" in volume_create
        assert "--fs-type" not in volume_create
        assert calls.index(volume_create) < next(
            index
            for index, call in enumerate(calls)
            if "compute droplet create" in call
        )

        droplet_create = _only(calls, "compute droplet create")
        assert "--region ams3" in droplet_create
        assert "--size s-4vcpu-8gb" in droplet_create
        assert "--image ubuntu-24-04-x64" in droplet_create
        assert "--volumes vol-123" in droplet_create
        assert "--tag-name slipfork-dev-vm" in droplet_create
        assert "--ssh-keys 902" in droplet_create
        user_data = (harness.state / "user-data.txt").read_text(encoding="utf-8")
        for needle in (
            "mkfs.btrfs",
            "user_subvol_rm_allowed,noatime",
            do_vm_mod.LOGIN_BANNER,
            do_vm_mod.RUSTUP_INIT_SHA256,
            do_vm_mod.MARKER,
            "_format_allowed=1",
        ):
            assert needle in user_data
        assert "docker.io" not in user_data
        assert "systemctl enable --now docker" not in user_data
        assert _deletions(calls) == []
        assert "$58.00/month" in result.stdout
        assert "tools/do_vm.py down --yes" in result.stdout
        assert "identity " in result.stdout

    def test_up_generates_persistent_identity_0600_in_0700_dir(self):
        harness = self.harness
        result = harness.run("up", "--yes")
        assert result.code == 0, result.stderr
        identity = do_vm_mod.default_identity_path()
        assert identity.is_file()
        assert identity.stat().st_mode & 0o777 == 0o600
        assert identity.parent.stat().st_mode & 0o777 == 0o700
        assert Path(f"{identity}.pub").is_file()
        before = identity.read_bytes()
        again = harness.run("up", "--yes")
        assert again.code == 0, again.stderr
        assert identity.read_bytes() == before
        calls = harness.ssh_calls()
        assert all(f"-i {identity}" in call for call in calls)

    def test_identity_default_ignores_xdg_cache_home(self):
        harness = self.harness
        tmp_path = self.tmp_path
        xdg = tmp_path / "xdg"
        result = harness.run("up", "--yes", env={"XDG_CACHE_HOME": str(xdg)})
        assert result.code == 0, result.stderr
        assert do_vm_mod.default_identity_path().is_file()
        assert not xdg.exists()

    def test_up_imports_bootstrap_key_when_doctl_cannot_see_it(self):
        harness = self.harness
        result = harness.run("up", "--yes")
        assert result.code == 0, result.stderr
        imported = _only(harness.doctl_calls(), "compute ssh-key import")
        assert "--public-key-file" in imported
        assert "slipfork-" in imported
        assert "--ssh-keys 902" in _only(
            harness.doctl_calls(), "compute droplet create"
        )

    def test_up_fails_closed_when_bootstrap_key_stays_invisible(self):
        harness = self.harness
        result = harness.run(
            "up",
            "--yes",
            env={"FAKE_IMPORT_FAIL": "422 SSH Key is already in use on your account"},
        )
        assert result.code == 2
        assert "could not be imported" in result.stderr
        assert "already in use" in result.stderr
        assert "owner-laptop" in result.stderr
        assert not any("create" in call for call in harness.doctl_calls())

    def test_up_fails_closed_when_import_succeeds_but_key_stays_invisible(self):
        harness = self.harness
        result = harness.run("up", "--yes", env={"FAKE_IMPORT_INVISIBLE": "1"})
        assert result.code == 2
        assert "reported a successful import" in result.stderr
        assert "not visible after 0 seconds" in result.stderr
        assert not any("create" in call for call in harness.doctl_calls())

    def test_up_installs_authorized_keys_union_for_root_and_slipfork(self):
        harness = self.harness
        tmp_path = self.tmp_path
        extra_identity = tmp_path / "extra" / "id_ed25519"
        do_vm_mod.load_identity(extra_identity, materialize=True)
        owner = harness.key_file()
        result = harness.run(
            "up",
            "--yes",
            "--identity",
            str(extra_identity),
            "--authorized-key",
            str(owner),
        )
        assert result.code == 0, result.stderr
        bootstrap = Path(f"{do_vm_mod.default_identity_path()}.pub").read_text()
        extra = Path(f"{extra_identity}.pub").read_text()
        for name in ("root_authorized_keys", "slipfork_authorized_keys"):
            content = (harness.state / name).read_text(encoding="utf-8")
            assert bootstrap.split()[1] in content
            assert extra.split()[1] in content
            assert "owner@example" in content
        script = (harness.state / "install-keys.sh").read_text(encoding="utf-8")
        assert (
            "set -- root /root/.ssh/authorized_keys slipfork "
            "/home/slipfork/.ssh/authorized_keys" in script
        )
        assert 'while [ "$#" -gt 0 ]' in script
        assert "sort -u" in script

    def test_up_reuses_tagged_droplet_and_volume(self):
        harness = self.harness
        harness.existing()
        result = harness.run("up", "--yes")
        assert result.code == 0, result.stderr
        assert "reusing droplet 9001" in result.stdout
        assert "existing tagged droplet/volume win" in result.stdout
        assert not any("create" in call for call in harness.doctl_calls())
        assert not any("volume-action attach" in call for call in harness.doctl_calls())

    def test_up_recreates_droplet_on_same_volume_when_identity_is_rejected(self):
        harness = self.harness
        harness.existing()
        result = harness.run(
            "up",
            "--yes",
            env={
                "FAKE_READY_EXIT": "255",
                "FAKE_READY_STDERR": (
                    "root@203.0.113.10: Permission denied (publickey)."
                ),
            },
        )
        assert result.code == 0, result.stderr
        assert "recreate droplet 9001" in result.stdout
        calls = harness.doctl_calls()
        detach = _only(calls, "compute volume-action detach")
        assert "vol-123" in detach
        assert "9001" in detach
        delete = _only(calls, "compute droplet delete")
        assert "9001" in delete
        assert calls.index(detach) < calls.index(delete)
        create = _only(calls, "compute droplet create")
        assert "--volumes vol-123" in create
        assert not any("compute volume create" in call for call in calls)
        assert not any("compute volume delete" in call for call in calls)

    def test_up_does_not_recreate_on_host_key_change(self):
        harness = self.harness
        harness.existing()
        result = harness.run(
            "up",
            "--yes",
            env={
                "FAKE_READY_EXIT": "255",
                "FAKE_READY_STDERR": "@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!",
            },
        )
        assert result.code == 2
        assert "host-key check failed" in result.stderr
        assert _deletions(harness.doctl_calls()) == []

    def test_up_reconciles_volume_only_by_creating_the_droplet_with_it(self):
        harness = self.harness
        harness.write_state("volume.json", _volume_state())
        result = harness.run("up", "--yes")
        assert result.code == 0, result.stderr
        calls = harness.doctl_calls()
        assert "--volumes vol-123" in _only(calls, "compute droplet create")
        assert not any("compute volume create" in call for call in calls)

    def test_up_format_decision_reaches_cloud_init(self):
        harness = self.harness
        harness.write_state("volume.json", _volume_state())
        without_flag = harness.run("up", "--yes")
        assert without_flag.code == 0, without_flag.stderr
        assert "_format_allowed=0" in (harness.state / "user-data.txt").read_text(
            encoding="utf-8"
        )

        (harness.state / "droplet.json").unlink()
        (harness.state / "created").unlink()
        with_flag = harness.run("up", "--yes", "--format-blank")
        assert with_flag.code == 0, with_flag.stderr
        assert "_format_allowed=1" in (harness.state / "user-data.txt").read_text(
            encoding="utf-8"
        )

    def test_up_reconciles_droplet_only_by_attaching_the_volume(self):
        harness = self.harness
        unattached = _droplet_state()
        del unattached[0]["volume_ids"]
        harness.write_state("droplet.json", unattached)
        result = harness.run("up", "--yes")
        assert result.code == 0, result.stderr
        attach = _only(harness.doctl_calls(), "compute volume-action attach")
        assert "vol-123" in attach
        assert "9001" in attach
        assert not any(
            "compute droplet create" in call for call in harness.doctl_calls()
        )

    def test_up_refuses_non_interactive_without_yes(self):
        harness = self.harness
        result = harness.run("up")
        assert result.code == 2
        assert "non-interactive" in result.stderr
        assert not any("create" in call for call in harness.doctl_calls())
        assert not any("ssh-key import" in call for call in harness.doctl_calls())

    def test_up_rejects_duplicate_tags_and_foreign_region(self):
        harness = self.harness
        harness.write_state("droplet.json", _droplet_state() + _droplet_state())
        duplicate = harness.run("up", "--yes")
        assert duplicate.code == 2
        assert "2 droplets carry tag" in duplicate.stderr

        foreign = _droplet_state()
        foreign[0]["region"] = {"slug": "lon1"}
        harness.write_state("droplet.json", foreign)
        harness.write_state("volume.json", _volume_state())
        result = harness.run("up", "--yes")
        assert result.code == 2
        assert "lon1" in result.stderr and "ams3" in result.stderr
        assert not any("create" in call for call in harness.doctl_calls())

    def test_up_rejects_volume_below_minimum(self):
        harness = self.harness
        result = harness.run("up", "--yes", "--volume-gb", "5")
        assert result.code == 2
        assert "at least 10" in result.stderr

    def test_up_fails_closed_when_size_price_is_missing(self):
        harness = self.harness
        result = harness.run("up", "--yes", env={"FAKE_SIZE_PRICE_NULL": "1"})
        assert result.code == 2
        assert "no monthly price" in result.stderr
        assert not any("create" in call for call in harness.doctl_calls())

    def test_up_cloud_init_timeout_prints_logs_and_deletes_nothing(self):
        harness = self.harness
        harness.existing()
        result = harness.run(
            "up",
            "--yes",
            env={
                "FAKE_READY_EXIT": "1",
                "FAKE_READY_STDERR": "cloud-init boom",
                "FAKE_CLOUD_INIT_RUNNING": "1",
            },
        )
        assert result.code == 2
        assert "did not finish" in result.stderr
        assert "cloud-init boom" in result.stderr
        assert "cloud-init-output.log" in result.stderr
        assert _deletions(harness.doctl_calls()) == []

    def test_up_reports_a_provisioning_refusal_not_a_transport_failure(self):
        harness = self.harness
        harness.existing()
        result = harness.run(
            "up",
            "--yes",
            env={
                "FAKE_READY_EXIT": "1",
                "FAKE_PROVISION_EXIT": "1",
                "FAKE_PROVISION_STDERR": (
                    "slipfork: blank volume detected; re-run with --format-blank after "
                    "confirming it holds no data"
                ),
            },
        )
        assert result.code == 1
        assert "provisioning on 203.0.113.10 failed (1)" in result.stderr
        assert "blank volume detected" in result.stderr
        assert "cannot reach" not in result.stderr

    def test_up_reports_ssh_255_as_a_transport_failure(self):
        harness = self.harness
        harness.existing()
        result = harness.run(
            "up",
            "--yes",
            env={
                "FAKE_READY_EXIT": "1",
                "FAKE_PROVISION_EXIT": "255",
                "FAKE_PROVISION_STDERR": (
                    "ssh: connect to host 203.0.113.10 port 22: Connection refused"
                ),
            },
        )
        assert result.code == 2
        assert "cannot reach 203.0.113.10 over SSH" in result.stderr
        assert "provisioning on" not in result.stderr

    def test_status_prints_fingerprints_and_fails_closed_when_probe_fails(self):
        harness = self.harness
        harness.existing()
        harness.materialize_identity()
        ok = harness.run("status")
        assert ok.code == 0, ok.stderr
        assert "SHA256:FAKEKEY" in ok.stdout
        assert "203.0.113.10" in ok.stdout

        failed = harness.run("status", env={"FAKE_FINGERPRINT_EXIT": "1"})
        assert failed.code == 2
        assert "could not read authorized-key fingerprints" in failed.stderr

    def test_status_without_resources_is_clean_and_does_not_ssh(self):
        harness = self.harness
        harness.materialize_identity()
        result = harness.run("status")
        assert result.code == 0, result.stderr
        assert "none (tag slipfork-dev-vm)" in result.stdout
        assert harness.ssh_calls() == []

    def test_status_malformed_droplet_networks_fails_closed(self):
        harness = self.harness
        harness.materialize_identity()
        malformed = _droplet_state()
        malformed[0]["networks"] = {"v4": "not-a-list"}
        harness.write_state("droplet.json", malformed)
        result = harness.run("status")
        assert result.code == 2
        assert "malformed networks.v4" in result.stderr
        assert "Traceback" not in result.stderr
        assert not any("create" in call for call in harness.doctl_calls())

    def test_identity_flag_is_repeatable_and_first_wins(self):
        harness = self.harness
        tmp_path = self.tmp_path
        harness.write_state("droplet.json", _droplet_state())
        first = tmp_path / "first" / "id_ed25519"
        second = tmp_path / "second" / "id_ed25519"
        do_vm_mod.load_identity(first, materialize=True)
        do_vm_mod.load_identity(second, materialize=True)
        result = harness.run(
            "status", "--identity", str(first), "--identity", str(second)
        )
        assert result.code == 0, result.stderr
        calls = harness.ssh_calls()
        assert any(f"-i {first}" in call for call in calls)
        assert not any(f"-i {second}" in call for call in calls)

    def test_sync_and_down_reject_identity_flag(self):
        harness = self.harness
        for command in ("sync", "down"):
            result = harness.run(command, "--identity", "a")
            assert result.code == 2, command
            assert "--identity" in result.stderr

    def test_test_names_missing_suites_and_nodes(self):
        harness = self.harness
        result = harness.run("test")
        assert result.code == 2
        for needle in (
            "crates/slipfork-world/tests/critical_btrfs_live.rs",
            "crates/slipfork/tests/critical_world_journey.rs",
            "M3.39.1",
            "M3.39.5",
        ):
            assert needle in result.stderr
        assert harness.ssh_calls() == []

    def test_test_streams_every_suite_serialized_and_stops_at_first_failure(self):
        harness = self.harness
        harness.suite_files()
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        failed = harness.run("test", env={"FAKE_WORLD_EXIT": "101"})
        assert failed.code == 101
        suites = _lines(harness.state / "suites.log")
        assert len(suites) == 1
        assert (
            "cargo test -p slipfork-world --features slipfork-world/live "
            "--test critical_btrfs_live"
        ) in suites[0]
        assert "-- --test-threads=1" in suites[0]
        assert "--nocapture" not in suites[0]
        assert "HOME=/var/lib/slipfork/home" in suites[0]
        assert "TMPDIR=/var/lib/slipfork/home/tmp" in suites[0]
        assert "XDG_RUNTIME_DIR=/dev/shm/slipfork-runtime" in suites[0]
        assert "SLIPFORK_TEST_ROOT=/var/lib/slipfork/t" in suites[0]
        assert "runuser -u slipfork" in suites[0]
        assert any("command -v docker" in call for call in harness.ssh_calls())

        (harness.state / "suites.log").unlink()
        ok = harness.run("test")
        assert ok.code == 0, ok.stderr
        suites = _lines(harness.state / "suites.log")
        assert len(suites) == len(do_vm_mod.SUITES) > 0
        expected = [
            f"cargo test -p {package} --features {package}/live "
            f"--test {test} -- --test-threads=1"
            for package, test, _, _ in do_vm_mod.SUITES
        ]
        for command, needle in zip(suites, expected, strict=True):
            assert needle in command

    def test_test_fails_closed_when_remote_suite_is_missing(self):
        harness = self.harness
        harness.suite_files()
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        result = harness.run("test", env={"FAKE_REMOTE_SUITE_EXIT": "1"})
        assert result.code == 2
        assert "run `tools/do_vm.py sync` first" in result.stderr
        assert not (harness.state / "suites.log").exists()

    def test_test_fails_closed_when_cargo_is_too_old(self):
        harness = self.harness
        harness.suite_files()
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        result = harness.run(
            "test",
            env={"FAKE_CARGO_VERSION": "cargo 1.93.0 (0000000 2025-11-01)"},
        )
        assert result.code == 2
        assert "older than 1.94.0" in result.stderr
        assert not (harness.state / "suites.log").exists()

    def test_sync_refuses_env_files_before_any_ssh(self):
        harness = self.harness
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        (harness.root / ".env").write_text("SECRET=1\n", encoding="utf-8")
        result = harness.run("sync")
        assert result.code == 2
        assert "refusing to sync secret environment file(s)" in result.stderr
        assert harness.ssh_calls() == []

    def test_sync_tar_pipes_working_tree_into_staging(self):
        harness = self.harness
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        (harness.root / ".env.example").write_text("SECRET=\n", encoding="utf-8")
        (harness.root / "README.md").write_text("hello\n", encoding="utf-8")
        result = harness.run("sync")
        assert result.code == 0, result.stderr
        calls = harness.ssh_calls()
        assert any("rm -rf /home/slipfork/.sync" in call for call in calls)
        assert any("tar -x -C /home/slipfork/.sync" in call for call in calls)
        assert any(
            "chown -R slipfork:slipfork /home/slipfork/slipfork" in call
            for call in calls
        )
        extracted = harness.state / "remote"
        assert (extracted / "README.md").read_text(encoding="utf-8") == "hello\n"
        assert (extracted / ".env.example").is_file()
        assert "synced 2 file(s)" in result.stdout

    def test_down_keeps_volume_by_default_and_requires_yes(self):
        harness = self.harness
        harness.existing()
        identity = harness.materialize_identity()
        refused = harness.run("down")
        assert refused.code == 2
        assert "non-interactive" in refused.stderr
        assert _deletions(harness.doctl_calls()) == []

        confirmed = harness.run("down", "--yes")
        assert confirmed.code == 0, confirmed.stderr
        calls = harness.doctl_calls()
        assert len([call for call in calls if "compute droplet delete" in call]) == 1
        assert not any("compute volume delete" in call for call in calls)
        assert (harness.state / "volume.json").is_file()
        assert identity.is_file()
        assert "destroyed 1 droplet(s) and 0 volume(s)" in confirmed.stdout
        assert "kept 1 tagged volume(s)" in confirmed.stdout
        assert not any("root@" in call for call in harness.ssh_calls())

    def test_down_delete_volume_destroys_droplet_then_volume(self):
        harness = self.harness
        harness.existing()
        confirmed = harness.run("down", "--yes", "--delete-volume")
        assert confirmed.code == 0, confirmed.stderr
        calls = harness.doctl_calls()
        droplet_index = next(
            index
            for index, call in enumerate(calls)
            if "compute droplet delete" in call
        )
        volume_index = next(
            index for index, call in enumerate(calls) if "compute volume delete" in call
        )
        assert droplet_index < volume_index
        assert "9001" in calls[droplet_index]
        assert "vol-123" in calls[volume_index]
        assert not (harness.state / "volume.json").exists()
        assert "destroyed 1 droplet(s) and 1 volume(s)" in confirmed.stdout

    def test_down_reports_survivors(self):
        harness = self.harness
        harness.write_state("droplet.json", _droplet_state())
        result = harness.run("down", "--yes", env={"FAKE_DELETE_STUCK": "1"})
        assert result.code == 2
        assert "survived deletion" in result.stderr
        assert "droplet 9001" in result.stderr

    def test_ssh_uses_identity_and_owner_envelope(self):
        harness = self.harness
        monkeypatch = self.monkeypatch
        harness.write_state("droplet.json", _droplet_state())
        identity = harness.materialize_identity()
        recorded: list[list[str]] = []
        monkeypatch.setattr(
            do_vm_mod.os, "execvp", lambda _file, argv: recorded.append(list(argv))
        )
        result = harness.run("ssh")
        assert result.code == 0, result.stderr
        argv = recorded[0]
        assert argv[0] == "ssh"
        assert f"-i {identity}" in " ".join(argv)
        assert "-t" in argv
        assert "BatchMode=yes" not in argv
        remote = argv[-1]
        assert "HOME=/var/lib/slipfork/root-home" in remote
        assert "TMPDIR=/var/lib/slipfork/root-home/tmp" in remote
        assert "CARGO_HOME=/home/slipfork/.cargo" in remote
        assert "RUSTUP_HOME=/home/slipfork/.rustup" in remote
        assert "exec bash --noprofile --norc" in remote
        assert "command -v slipfork" in remote

    def test_ssh_forwards_remote_command(self):
        harness = self.harness
        monkeypatch = self.monkeypatch
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        recorded: list[list[str]] = []
        monkeypatch.setattr(
            do_vm_mod.os, "execvp", lambda _file, argv: recorded.append(list(argv))
        )
        result = harness.run(
            "ssh", "--", "findmnt", "-no", "OPTIONS", "/var/lib/slipfork"
        )
        assert result.code == 0, result.stderr
        argv = recorded[0]
        assert "-t" not in argv
        remote = argv[-1]
        assert "findmnt -no OPTIONS /var/lib/slipfork" in remote
        assert "command -v slipfork" not in remote
        assert "exec bash" not in remote
        assert "HOME=/var/lib/slipfork/root-home" in remote

    def test_no_shell_true_anywhere(self):
        harness = self.harness
        monkeypatch = self.monkeypatch
        assert "shell=True" not in TOOL.read_text(encoding="utf-8")
        recorded: list[dict[str, object]] = []
        real_run = subprocess.run

        def spy(*args: object, **kwargs: object) -> object:
            recorded.append(kwargs)
            return real_run(*args, **kwargs)

        monkeypatch.setattr(do_vm_mod.subprocess, "run", spy)
        result = harness.run("up", "--yes")
        assert result.code == 0, result.stderr
        assert recorded
        assert all("shell" not in kwargs for kwargs in recorded)

    def test_provision_script_is_valid_bash(self):
        for renderer in (
            do_vm_mod.render_provision_script,
            do_vm_mod.render_volume_discovery,
            do_vm_mod.render_automount_teardown,
            do_vm_mod.render_volume_signature_step,
            do_vm_mod.render_install_authorized_keys,
            do_vm_mod.render_product_install,
            do_vm_mod.render_demo_seed,
            do_vm_mod.render_login_banner,
        ):
            result = subprocess.run(
                ["bash", "-n"],
                input=renderer(),
                text=True,
                capture_output=True,
                check=False,
            )
            assert result.returncode == 0, result.stderr

    def test_volume_discovery_selects_the_disk_and_ignores_partitions(self):
        tmp_path = self.tmp_path
        volumes = tmp_path / "volumes"
        volumes.mkdir()
        disk = volumes / "scsi-0DO_Volume_slipfork-dev-vm"
        disk.write_text("", encoding="utf-8")
        (volumes / "scsi-0DO_Volume_slipfork-dev-vm-part1").write_text(
            "", encoding="utf-8"
        )
        (volumes / "scsi-0DO_Volume_slipfork-dev-vm-part9").write_text(
            "", encoding="utf-8"
        )
        config = do_vm_mod.ProvisionConfig(
            volume_glob=str(volumes / "scsi-0DO_Volume_*"),
            device_wait_attempts=1,
            device_wait_seconds=0,
        )
        script = (
            do_vm_mod.render_volume_discovery(config)
            + '\nprintf "%s" "$volume_device"\n'
        )
        selected = run_bash(script, tmp_path / "bin")
        assert selected.returncode == 0, selected.stderr
        assert selected.stdout == str(disk)

        (volumes / "scsi-0DO_Volume_other").write_text("", encoding="utf-8")
        ambiguous = run_bash(script, tmp_path / "bin")
        assert ambiguous.returncode == 1
        assert "exactly one attached DO volume disk" in ambiguous.stderr
        assert "scsi-0DO_Volume_other" in ambiguous.stderr

    def test_cloud_init_embeds_provision_script_verbatim(self):
        script = do_vm_mod.render_provision_script()
        cloud = do_vm_mod.cloud_init()
        dedented = "\n".join(line[6:] for line in cloud.splitlines()[5:])
        assert dedented.rstrip("\n") == script.rstrip("\n")
        assert "mkfs.btrfs" in cloud
        assert do_vm_mod.MARKER in cloud

    def test_cloud_init_carries_the_format_decision(self):
        assert "_format_allowed=0" in do_vm_mod.cloud_init()
        assert "_format_allowed=1" in do_vm_mod.cloud_init(
            do_vm_mod.ProvisionConfig(allow_format=True)
        )

    def test_volume_signature_step_branches_on_blkid(self):
        tmp_path = self.tmp_path
        cases = [
            (
                {"FAKE_BLKID_RC": "0", "FAKE_BLKID_TYPE": "btrfs"},
                b"",
                False,
                0,
                [],
                False,
            ),
            (
                {"FAKE_BLKID_RC": "0", "FAKE_BLKID_TYPE": "ext4"},
                b"",
                True,
                1,
                ["ext4", "refusing to format"],
                False,
            ),
            (
                {"FAKE_BLKID_RC": "0", "FAKE_BLKID_TYPE": ""},
                b"",
                True,
                1,
                ["no filesystem type"],
                False,
            ),
            (
                {"FAKE_BLKID_RC": "2", "FAKE_BLKID_TYPE": ""},
                b"\0" * MIB,
                False,
                1,
                ["blank volume detected; re-run with --format-blank"],
                False,
            ),
            (
                {"FAKE_BLKID_RC": "2", "FAKE_BLKID_TYPE": ""},
                ZERO_8MIB,
                True,
                0,
                [],
                True,
            ),
            (
                {"FAKE_BLKID_RC": "2", "FAKE_BLKID_TYPE": ""},
                NONZERO_IN_MIB_1_TO_8,
                True,
                1,
                ["non-zero data in its first 8 MiB"],
                False,
            ),
            (
                {
                    "FAKE_BLKID_RC": "2",
                    "FAKE_BLKID_TYPE": "",
                    "FAKE_WIPEFS_OUT": "0x83 ext4",
                },
                ZERO_8MIB,
                True,
                1,
                ["wipefs", "signatures"],
                False,
            ),
            (
                {
                    "FAKE_BLKID_RC": "2",
                    "FAKE_BLKID_TYPE": "",
                    "FAKE_DD_RC": "1",
                    "FAKE_DD_STDERR": "Input/output error",
                },
                ZERO_8MIB,
                True,
                1,
                ["first 8 MiB", "rc 1", "Input/output error"],
                False,
            ),
            (
                {"FAKE_BLKID_RC": "4", "FAKE_BLKID_ERR": "blkid: probe failed"},
                b"",
                True,
                1,
                ["blkid failed", "rc 4", "probe failed"],
                False,
            ),
            (
                {"FAKE_BLKID_RC": "8", "FAKE_BLKID_ERR": "ambivalent"},
                b"",
                True,
                1,
                ["blkid failed", "rc 8", "ambivalent"],
                False,
            ),
            (
                {
                    "FAKE_BLKID_RC": "0",
                    "FAKE_BLKID_TYPE": "btrfs",
                    "FAKE_BLKID_UUID_RC": "2",
                },
                b"",
                False,
                1,
                ["could not read the filesystem UUID"],
                False,
            ),
        ]
        for index, case in enumerate(cases):
            env, device_bytes, allow_format, expected_code, needles, formats = case
            with self.subTest(env=env):
                tmp_path = self.tmp_path / f"case{index}"
                tmp_path.mkdir()
                device = tmp_path / "device"
                device.write_bytes(device_bytes)
                result = _run_signature_step(
                    tmp_path, device, env, allow_format=allow_format
                )
                assert result.returncode == expected_code, result.stderr
                for needle in needles:
                    assert needle in result.stderr
                formats_called = _mkfs_calls(tmp_path)
                assert bool(formats_called) is formats
                if formats:
                    assert len(formats_called) == 1
                    assert "-f -L slipfork" in formats_called[0]
                    assert "blank volume evidence" in result.stdout
                    assert "first 8 MiB=all zero" in result.stdout

    def test_automount_teardown_is_conditional_and_fails_closed(self):
        tmp_path = self.tmp_path
        bin_dir = tmp_path / "fakebin"
        bin_dir.mkdir()
        log = tmp_path / "commands.log"
        _write_executable(
            bin_dir / "systemctl",
            """#!/bin/sh
        printf 'systemctl %s\\n' "$*" >> "$FAKE_COMMAND_LOG"
        case "$*" in
          *list-units*)
        if [ -n "${FAKE_SYSTEMCTL_STDERR:-}" ]; then
          printf '%s\\n' "$FAKE_SYSTEMCTL_STDERR" >&2
        fi
        if [ "${FAKE_SYSTEMCTL_EXIT:-0}" != "0" ]; then
          exit "$FAKE_SYSTEMCTL_EXIT"
        fi
        if [ "${FAKE_UNIT:-}" = "1" ]; then
          printf 'mnt-volume_abc.mount loaded active mounted /mnt/volume_abc\\n'
        fi ;;
        esac
        exit 0
        """,
        )
        _write_executable(
            bin_dir / "findmnt",
            """#!/bin/sh
        printf 'findmnt %s\\n' "$*" >> "$FAKE_COMMAND_LOG"
        if [ "${FAKE_MOUNTED:-}" = "1" ]; then
          printf '/mnt/volume_abc\\n'
          exit 0
        fi
        exit 1
        """,
        )
        _write_executable(
            bin_dir / "umount",
            '#!/bin/sh\nprintf \'umount %s\\n\' "$*" >> "$FAKE_COMMAND_LOG"\n',
        )
        _write_executable(
            bin_dir / "udevadm",
            '#!/bin/sh\nprintf \'udevadm %s\\n\' "$*" >> "$FAKE_COMMAND_LOG"\n',
        )

        units = tmp_path / "units"
        units.mkdir()
        (units / "mnt-volume_abc.mount").write_text("unit\n", encoding="utf-8")
        fstab = tmp_path / "fstab"
        fstab.write_text(
            "/dev/sdc1 /mnt/other ext4 defaults 0 0\n/dev/sdb1 /data ext4 defaults 0 0\n",
            encoding="utf-8",
        )
        rule = tmp_path / "99-digitalocean-automount.rules"
        rule.write_text("rule\n", encoding="utf-8")
        config = do_vm_mod.ProvisionConfig(
            fstab=str(fstab), udev_rule=str(rule), unit_dirs=(str(units),)
        )
        script = (
            "volume_device=/dev/disk/by-id/scsi-0DO_Volume_abc\n"
            + do_vm_mod.render_automount_teardown(config)
        )

        blank = run_bash(script, bin_dir, FAKE_COMMAND_LOG=str(log))
        assert blank.returncode == 0, blank.stderr
        commands = log.read_text(encoding="utf-8")
        assert "systemctl disable --now" not in commands
        assert "umount" not in commands
        assert fstab.read_text(encoding="utf-8").count("/mnt/other") == 1

        fstab.write_text(
            "/dev/disk/by-id/scsi-0DO_Volume_abc /mnt/volume_abc ext4 defaults 0 0\n"
            "/dev/sdb1 /data ext4 defaults 0 0\n",
            encoding="utf-8",
        )
        preformatted = run_bash(
            script,
            bin_dir,
            FAKE_COMMAND_LOG=str(log),
            FAKE_UNIT="1",
            FAKE_MOUNTED="1",
        )
        assert preformatted.returncode == 0, preformatted.stderr
        commands = log.read_text(encoding="utf-8")
        assert "systemctl disable --now mnt-volume_abc.mount" in commands
        assert "umount /mnt/volume_abc" in commands
        assert "udevadm control --reload-rules" in commands
        assert not rule.exists()
        assert not (units / "mnt-volume_abc.mount").exists()
        remaining = fstab.read_text(encoding="utf-8")
        assert "/mnt/volume_abc" not in remaining
        assert "/data" in remaining

        failed = run_bash(
            script,
            bin_dir,
            FAKE_COMMAND_LOG=str(log),
            FAKE_SYSTEMCTL_EXIT="1",
            FAKE_SYSTEMCTL_STDERR="systemctl is broken",
        )
        assert failed.returncode == 1
        assert "systemctl list-units failed" in failed.stderr
        assert "systemctl is broken" in failed.stderr

    def test_provision_script_installs_no_host_crun_and_keeps_git(self):
        script = do_vm_mod.render_provision_script()
        assert "crun" not in do_vm_mod.PACKAGES.split()
        install = next(
            line for line in script.splitlines() if "apt-get install" in line
        )
        assert "crun" not in install.split()
        assert ".slipfork/bin/crun}" not in script
        assert "profile slipfork" not in script
        assert "apparmor_parser -r" not in script
        assert "zfs" not in script.lower()
        assert "docker.io" not in script
        assert "systemctl enable --now docker" not in script
        assert "docker buildx" not in script
        assert "btrfs-progs" in script
        assert "git" in script
        assert do_vm_mod.RUSTUP_INIT_SHA256 in script
        assert '_rustup_init="$_rustup_dir/rustup-init"' in script
        assert "user_subvol_rm_allowed,noatime" in script
        assert "9123683e" in script
        assert "_format_allowed=0" in script

    def test_provisioning_clears_the_apparmor_userns_restriction(self):
        script = do_vm_mod.render_provision_script()
        assert "/etc/sysctl.d/99-slipfork-userns.conf" in script
        assert "kernel.apparmor_restrict_unprivileged_userns=0" in script
        assert "sysctl -w kernel.apparmor_restrict_unprivileged_userns=0" in script
        assert (
            "runuser -u slipfork -- unshare --user --map-current-user --keep-caps --net -- true"
            in script
        ), "provisioning must prove keep-caps userns creation"
        assert "unprivileged user namespaces are denied" in script

    def test_install_authorized_keys_targets_both_users(self):
        script = do_vm_mod.render_install_authorized_keys()
        assert (
            "set -- root /root/.ssh/authorized_keys slipfork "
            "/home/slipfork/.ssh/authorized_keys" in script
        )
        assert 'while [ "$#" -gt 0 ]' in script
        assert "sort -u" in script
        assert script.count("authorized_keys") >= 3

    def test_secrets_and_private_key_never_logged(self):
        harness = self.harness
        owner = harness.key_file()
        (harness.root / ".env").write_text("SECRET_SENTINEL=1\n", encoding="utf-8")
        result = harness.run("up", "--yes", "--authorized-key", str(owner))
        assert result.code == 0, result.stderr
        private_key = do_vm_mod.default_identity_path().read_text(encoding="utf-8")
        haystacks = [
            result.stdout,
            result.stderr,
            harness.doctl_log.read_text(encoding="utf-8"),
            harness.ssh_log.read_text(encoding="utf-8"),
            (harness.state / "user-data.txt").read_text(encoding="utf-8"),
        ]
        for haystack in haystacks:
            assert private_key.strip() not in haystack
            assert "SECRET_SENTINEL" not in haystack

    def test_product_install_hands_port_80_to_daemon_install(self):
        script = do_vm_mod.render_product_install()
        for binary in do_vm_mod.PRODUCT_BINARIES:
            assert f'target/{do_vm_mod.BUILD_PROFILE}/"$_binary"' in script
        assert f"for _binary in {' '.join(do_vm_mod.PRODUCT_BINARIES)}; do" in script
        assert 'if ! cmp -s "$_source"' in script
        assert "/usr/local/bin/slipfork daemon install --ingress" in script
        assert (
            'SUDO_UID="$_slipfork_uid" SUDO_GID="$(id -g slipfork)" '
            "/usr/local/bin/slipfork daemon install --ingress"
        ) in script
        assert "loginctl enable-linger slipfork" in script
        assert "systemctl --user enable --now slipfork" in script
        assert (
            "/usr/local/bin/slipfork daemon install --help >/dev/null 2>&1"
        ) in script
        assert "this build has no `daemon` mode yet" in script
        assert "systemctl is-active slipfork-ingress.socket" in script
        assert "systemctl --user is-active slipfork" in script
        assert '!= "active"' in script
        assert "aa-status" in script
        assert "runuser -u slipfork -- unshare --user" in script
        assert "|| true" not in script

    def test_product_build_is_derived_from_cargo_metadata(self):
        metadata = json.dumps(
            {
                "packages": [
                    {
                        "name": "slipfork",
                        "targets": [
                            {"name": "slipfork", "kind": ["bin"]},
                            {"name": "slipfork", "kind": ["test"]},
                        ],
                    },
                    {
                        "name": "slipfork-world",
                        "targets": [{"name": "slipfork-world-init", "kind": ["bin"]}],
                    },
                    {
                        "name": "slipfork-ingress",
                        "targets": [{"name": "not-a-product-bin", "kind": ["bin"]}],
                    },
                ]
            }
        )
        binaries = do_vm_mod.parse_product_binaries(metadata)
        assert binaries == ["slipfork", "slipfork-world-init"]
        assert set(binaries) == set(do_vm_mod.PRODUCT_BINARIES)
        assert do_vm_mod.product_build_command(binaries, "debug") == (
            "cargo build -p slipfork -p slipfork-world "
            "--bin slipfork --bin slipfork-world-init"
        )
        assert do_vm_mod.product_build_command(binaries, "release").startswith(
            "cargo build --release -p slipfork -p slipfork-world "
        )
        for broken, message in (
            ("", "did not emit JSON"),
            ("{}", "no `packages` list"),
            (
                json.dumps({"packages": [{"name": "slipfork", "targets": []}]}),
                "no binary targets",
            ),
        ):
            with self.assertRaisesRegex(do_vm_mod.HarnessError, message):
                do_vm_mod.parse_product_binaries(broken)
        with self.assertRaisesRegex(
            do_vm_mod.HarnessError, "unsupported product build profile"
        ):
            do_vm_mod.product_build_command(binaries, "bench")

    def test_sync_fails_closed_when_cargo_metadata_disagrees(self):
        harness = self.harness
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        (harness.root / "README.md").write_text("hello\n", encoding="utf-8")
        drifted = json.dumps(
            {
                "packages": [
                    {
                        "name": "slipfork",
                        "targets": [
                            {"name": "slipfork", "kind": ["bin"]},
                        ],
                    }
                ]
            }
        )
        result = harness.run("sync", env={"FAKE_METADATA_JSON": drifted})
        assert result.code == 2
        assert "cargo metadata disagrees with the shipped binary set" in result.stderr
        assert "slipfork-world-init" in result.stderr
        assert not (harness.state / "builds.log").exists()

    def test_product_install_copies_verifies_and_reruns_without_writes(self):
        tmp_path = self.tmp_path
        installed = tmp_path / "usr-local-bin"
        installed.mkdir()
        config = _demo_config(tmp_path, install_bin_dir=str(installed))
        sources = _product_binaries_tree(config)
        _write_executable(sources / "slipfork", FAKE_SLIPFORK)
        bin_dir = _fake_bin(tmp_path)

        first = _run_step(do_vm_mod.render_product_install(config), bin_dir, tmp_path)
        assert first.returncode == 0, first.stderr
        commands = _lines(tmp_path / "commands.log")
        installs = [entry for entry in commands if entry.startswith("install ")]
        assert len(installs) == len(do_vm_mod.PRODUCT_BINARIES)
        for entry in installs:
            assert "-m 0755" in entry
        assert "slipfork daemon install --help" in commands
        assert "slipfork daemon install" in commands
        assert "slipfork daemon install --ingress" in commands
        assert any(entry.startswith("loginctl enable-linger") for entry in commands)
        for binary in do_vm_mod.PRODUCT_BINARIES:
            assert (installed / binary).is_file()

        (tmp_path / "commands.log").write_text("", encoding="utf-8")
        second = _run_step(do_vm_mod.render_product_install(config), bin_dir, tmp_path)
        assert second.returncode == 0, second.stderr
        commands = _lines(tmp_path / "commands.log")
        assert [entry for entry in commands if entry.startswith("install ")] == []
        assert "slipfork daemon install --ingress" in commands

    def test_product_install_fails_closed_without_the_binaries(self):
        tmp_path = self.tmp_path
        config = _demo_config(tmp_path)
        bin_dir = _fake_bin(tmp_path)
        result = _run_step(do_vm_mod.render_product_install(config), bin_dir, tmp_path)
        assert result.returncode == 1
        assert "tools/do_vm.py test" in result.stderr
        assert "target/debug/slipfork" in result.stderr
        assert not (tmp_path / "commands.log").exists()

    def test_product_install_fails_closed_when_the_socket_is_not_active(self):
        tmp_path = self.tmp_path
        installed = tmp_path / "usr-local-bin"
        installed.mkdir()
        config = _demo_config(tmp_path, install_bin_dir=str(installed))
        _product_binaries_tree(config)
        bin_dir = _fake_bin(tmp_path)
        result = _run_step(
            do_vm_mod.render_product_install(config),
            bin_dir,
            tmp_path,
            FAKE_SYSTEMCTL_STATE="inactive",
        )
        assert result.returncode == 1
        assert "not active" in result.stderr
        assert "inactive" in result.stderr

    def test_product_install_fails_closed_when_the_profile_is_not_loaded(self):
        tmp_path = self.tmp_path
        installed = tmp_path / "usr-local-bin"
        installed.mkdir()
        config = _demo_config(tmp_path, install_bin_dir=str(installed))
        _product_binaries_tree(config)
        bin_dir = _fake_bin(tmp_path)
        result = _run_step(
            do_vm_mod.render_product_install(config),
            bin_dir,
            tmp_path,
            FAKE_AA_STATUS_FAIL="1",
        )
        assert result.returncode == 1
        assert "aa-status failed" in result.stderr

    def test_product_install_skips_socket_activation_without_a_daemon_verb(self):
        tmp_path = self.tmp_path
        installed = tmp_path / "usr-local-bin"
        installed.mkdir()
        config = _demo_config(tmp_path, install_bin_dir=str(installed))
        sources = _product_binaries_tree(config)
        _write_executable(sources / "slipfork", FAKE_SLIPFORK)
        bin_dir = _fake_bin(tmp_path)
        result = _run_step(
            do_vm_mod.render_product_install(config),
            bin_dir,
            tmp_path,
            FAKE_SLIPFORK_NO_DAEMON="1",
        )
        assert result.returncode == 0, result.stderr
        assert (
            "product install: this build has no `daemon` mode yet; "
            "skipping :80 socket activation" in result.stderr
        )
        commands = _lines(tmp_path / "commands.log")
        assert not any("--user" in entry for entry in commands)
        assert not any(entry.startswith("systemctl") for entry in commands)
        assert not any(entry.startswith("aa-status") for entry in commands)
        for binary in do_vm_mod.PRODUCT_BINARIES:
            assert (installed / binary).is_file()

    def test_product_install_propagates_a_real_daemon_install_failure(self):
        tmp_path = self.tmp_path
        installed = tmp_path / "usr-local-bin"
        installed.mkdir()
        config = _demo_config(tmp_path, install_bin_dir=str(installed))
        sources = _product_binaries_tree(config)
        _write_executable(sources / "slipfork", FAKE_SLIPFORK)
        bin_dir = _fake_bin(tmp_path)
        result = _run_step(
            do_vm_mod.render_product_install(config),
            bin_dir,
            tmp_path,
            FAKE_SLIPFORK_EXIT="2",
        )
        assert result.returncode == 2
        assert "skipping :80" not in result.stderr

    def test_demo_seed_copies_once_and_keeps_owner_edits(self):
        tmp_path = self.tmp_path
        config = _demo_config(tmp_path)
        demo = Path(config.remote_demo)
        bin_dir = _fake_bin(tmp_path)

        seeded = _run_step(do_vm_mod.render_demo_seed(config), bin_dir, tmp_path)
        assert seeded.returncode == 0, seeded.stderr
        assert "seeded" in seeded.stdout
        assert (demo / "snow.yml").is_file()
        assert (demo / "slipfork.toml").read_text(
            encoding="utf-8"
        ) == 'project = "demo"\n'
        stamp = demo / ".git" / do_vm_mod.DEMO_STAMP
        assert stamp.is_file()
        assert _git_demo(demo, "log", "--oneline").count("\n") == 1
        assert _git_demo(demo, "config", "user.name").strip() == do_vm_mod.DEMO_GIT_NAME
        assert (
            _git_demo(demo, "config", "user.email").strip() == do_vm_mod.DEMO_GIT_EMAIL
        )
        assert _git_demo(demo, "status", "--porcelain") == ""

        (demo / "README.md").write_text("owner edit\n", encoding="utf-8")
        again = _run_step(do_vm_mod.render_demo_seed(config), bin_dir, tmp_path)
        assert again.returncode == 0, again.stderr
        assert "unchanged" in again.stdout
        assert (demo / "README.md").read_text(encoding="utf-8") == "owner edit\n"

    def test_demo_seed_fails_closed_on_an_unstamped_directory(self):
        tmp_path = self.tmp_path
        config = _demo_config(tmp_path)
        demo = Path(config.remote_demo)
        demo.mkdir()
        (demo / "README.md").write_text("someone's work\n", encoding="utf-8")
        bin_dir = _fake_bin(tmp_path)
        result = _run_step(do_vm_mod.render_demo_seed(config), bin_dir, tmp_path)
        assert result.returncode == 1
        assert "move it aside" in result.stderr
        assert do_vm_mod.DEMO_STAMP in result.stderr
        assert (demo / "README.md").read_text(encoding="utf-8") == "someone's work\n"

    def test_demo_seed_skips_when_the_tree_has_no_template(self):
        tmp_path = self.tmp_path
        config = _demo_config(tmp_path)
        template = Path(config.remote_tree) / do_vm_mod.DEMO_TEMPLATE
        template.rename(tmp_path / "template-aside")
        result = _run_step(
            do_vm_mod.render_demo_seed(config), _fake_bin(tmp_path), tmp_path
        )
        assert result.returncode == 0, result.stderr
        assert "skipped" in result.stdout
        assert not Path(config.remote_demo).exists()

    def test_login_banner_states_the_account_and_the_shipped_verbs(self):
        banner = do_vm_mod.render_login_banner()
        for needle in (
            "account: slipfork",
            "cd ~/demo",
            "  slipfork\n",
            "  slipfork up\n",
            "  slipfork ps\n",
        ):
            assert needle in banner, banner
        # Guarded, not advertised before it exists: the demo directory and the installed CLI.
        assert f"[ -d {do_vm_mod.REMOTE_DEMO} ]" in banner
        assert "command -v slipfork" in banner
        script = do_vm_mod.render_provision_script()
        profile_d = set(re.findall(r"/etc/profile\.d/[\w.-]+", script))
        assert profile_d == {do_vm_mod.LOGIN_BANNER}
        assert script.count("<<'SLIPFORK_BANNER'") == 1
        assert f"cmp -s {do_vm_mod.LOGIN_BANNER} " in script

    def test_provisioning_links_the_cache_tree_at_the_volume(self):
        script = do_vm_mod.render_provision_script()
        assert f"ln -s {do_vm_mod.CACHE_TARGET} {do_vm_mod.CACHE_SYMLINK}" in script
        assert f"readlink {do_vm_mod.CACHE_SYMLINK}" in script
        assert f"stat -f -c %t {do_vm_mod.CACHE_SYMLINK}" in script
        assert "is a real directory" in script

    def test_no_getent_or_password_step_anywhere(self):
        for renderer in (
            do_vm_mod.render_provision_script,
            do_vm_mod.render_install_authorized_keys,
            do_vm_mod.render_product_install,
            do_vm_mod.render_demo_seed,
            do_vm_mod.render_login_banner,
        ):
            text = renderer()
            assert "getent" not in text, renderer.__name__
            assert "passwd" not in text.lower(), renderer.__name__

    def test_sync_seeds_the_demo_builds_and_installs_the_product(self):
        harness = self.harness
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        (harness.root / "README.md").write_text("hello\n", encoding="utf-8")
        result = harness.run("sync")
        assert result.code == 0, result.stderr
        calls = harness.ssh_calls()
        replace = next(
            index
            for index, call in enumerate(calls)
            if "chown -R slipfork:slipfork" in call
        )
        demo = calls.index(_only(calls, "bash -s demo-seed"))
        build = calls.index(
            _only(
                calls,
                "cargo build -p slipfork -p slipfork-world --bin slipfork --bin slipfork-world-init",
            )
        )
        install = calls.index(_only(calls, "bash -s product-install"))
        assert replace < demo < build < install
        assert "synced 1 file(s) to /home/slipfork/slipfork" in result.stdout
        demo_script = (harness.state / "demo-seed.sh").read_text(encoding="utf-8")
        assert do_vm_mod.DEMO_STAMP in demo_script
        assert f"{do_vm_mod.REMOTE_TREE}/{do_vm_mod.DEMO_TEMPLATE}" in demo_script
        install_script = (harness.state / "product-install.sh").read_text(
            encoding="utf-8"
        )
        assert "daemon install --ingress" in install_script

    def test_sync_fails_closed_when_the_product_install_refuses(self):
        harness = self.harness
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        (harness.root / "README.md").write_text("hello\n", encoding="utf-8")
        result = harness.run(
            "sync",
            env={
                "FAKE_PRODUCT_INSTALL_EXIT": "3",
                "FAKE_PRODUCT_INSTALL_STDERR": "slipfork: reported inactive, not active",
            },
        )
        assert result.code == 2
        assert "product install on 203.0.113.10 failed (3)" in result.stderr
        assert "not active" in result.stderr
        assert "synced" not in result.stdout

    def test_test_builds_and_installs_before_the_suites(self):
        harness = self.harness
        harness.suite_files()
        harness.write_state("droplet.json", _droplet_state())
        harness.materialize_identity()
        result = harness.run("test")
        assert result.code == 0, result.stderr
        calls = harness.ssh_calls()
        build = calls.index(
            _only(
                calls,
                "cargo build -p slipfork -p slipfork-world --bin slipfork --bin slipfork-world-init",
            )
        )
        install = calls.index(_only(calls, "bash -s product-install"))
        assert build < install
        assert "runuser -u slipfork" in calls[build]
        assert "HOME=/var/lib/slipfork/home" in calls[build]
        assert len(_lines(harness.state / "suites.log")) == len(do_vm_mod.SUITES)


if __name__ == "__main__":
    unittest.main()
