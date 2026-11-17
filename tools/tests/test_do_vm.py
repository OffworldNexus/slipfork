"""Unit tests for `tools/do_vm.py` driven by fake `doctl`/`ssh` executables on `PATH`.

No test touches the DigitalOcean API or a real VM: every cloud/SSH call lands in a script fake
that logs argv and keeps its state under `tmp_path`. The tool runs in-process so the bounded
waits can be patched; the bash renderers (`render_volume_discovery`,
`render_volume_signature_step`, `render_automount_teardown`) run for real under `bash` with fake
executables on `PATH`.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import shlex
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from unittest import mock

import pytest

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
if "docker info" in remote:
    sys.exit(env_int("FAKE_DOCKER_EXIT"))
if "cargo test -p " in remote:
    with (state / "suites.log").open("a", encoding="utf-8") as log:
        log.write(remote + "\\n")
    if "slipfork-engine" in remote:
        sys.exit(env_int("FAKE_ENGINE_EXIT"))
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


@pytest.fixture
def harness(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Harness:
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
    return Harness(root, home, state, doctl_log, ssh_log)


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


def test_help_lists_exactly_the_six_commands(harness: Harness) -> None:
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


def test_up_help_lists_identity_authorized_key_and_format_blank(
    harness: Harness,
) -> None:
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


def test_up_creates_volume_then_droplet_with_btrfs_provisioning(
    harness: Harness,
) -> None:
    result = harness.run("up", "--yes")
    assert result.code == 0, result.stderr
    calls = harness.doctl_calls()

    volume_create = _only(calls, "compute volume create")
    assert "--region ams3" in volume_create
    assert "--size 100GiB" in volume_create
    assert "--tag slipfork-dev-vm" in volume_create
    assert "--fs-type" not in volume_create
    assert calls.index(volume_create) < next(
        index for index, call in enumerate(calls) if "compute droplet create" in call
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
        "docker.io docker-buildx",
        "/etc/apparmor.d/slipfork",
        do_vm_mod.RUSTUP_INIT_SHA256,
        do_vm_mod.MARKER,
        "_format_allowed=1",
    ):
        assert needle in user_data
    assert _deletions(calls) == []
    assert "$58.00/month" in result.stdout
    assert "tools/do_vm.py down --yes" in result.stdout
    assert "identity " in result.stdout


def test_up_generates_persistent_identity_0600_in_0700_dir(
    harness: Harness,
) -> None:
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


def test_identity_default_ignores_xdg_cache_home(
    harness: Harness, tmp_path: Path
) -> None:
    xdg = tmp_path / "xdg"
    result = harness.run("up", "--yes", env={"XDG_CACHE_HOME": str(xdg)})
    assert result.code == 0, result.stderr
    assert do_vm_mod.default_identity_path().is_file()
    assert not xdg.exists()


def test_up_imports_bootstrap_key_when_doctl_cannot_see_it(
    harness: Harness,
) -> None:
    result = harness.run("up", "--yes")
    assert result.code == 0, result.stderr
    imported = _only(harness.doctl_calls(), "compute ssh-key import")
    assert "--public-key-file" in imported
    assert "slipfork-" in imported
    assert "--ssh-keys 902" in _only(harness.doctl_calls(), "compute droplet create")


def test_up_fails_closed_when_bootstrap_key_stays_invisible(
    harness: Harness,
) -> None:
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


def test_up_fails_closed_when_import_succeeds_but_key_stays_invisible(
    harness: Harness,
) -> None:
    result = harness.run("up", "--yes", env={"FAKE_IMPORT_INVISIBLE": "1"})
    assert result.code == 2
    assert "reported a successful import" in result.stderr
    assert "not visible after 0 seconds" in result.stderr
    assert not any("create" in call for call in harness.doctl_calls())


def test_up_installs_authorized_keys_union_for_root_and_slipfork(
    harness: Harness, tmp_path: Path
) -> None:
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
    assert "for _user in root slipfork" in script
    assert "sort -u" in script


def test_up_reuses_tagged_droplet_and_volume(harness: Harness) -> None:
    harness.existing()
    result = harness.run("up", "--yes")
    assert result.code == 0, result.stderr
    assert "reusing droplet 9001" in result.stdout
    assert "existing tagged droplet/volume win" in result.stdout
    assert not any("create" in call for call in harness.doctl_calls())
    assert not any("volume-action attach" in call for call in harness.doctl_calls())


def test_up_recreates_droplet_on_same_volume_when_identity_is_rejected(
    harness: Harness,
) -> None:
    harness.existing()
    result = harness.run(
        "up",
        "--yes",
        env={
            "FAKE_READY_EXIT": "255",
            "FAKE_READY_STDERR": ("root@203.0.113.10: Permission denied (publickey)."),
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


def test_up_does_not_recreate_on_host_key_change(harness: Harness) -> None:
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


def test_up_reconciles_volume_only_by_creating_the_droplet_with_it(
    harness: Harness,
) -> None:
    harness.write_state("volume.json", _volume_state())
    result = harness.run("up", "--yes")
    assert result.code == 0, result.stderr
    calls = harness.doctl_calls()
    assert "--volumes vol-123" in _only(calls, "compute droplet create")
    assert not any("compute volume create" in call for call in calls)


def test_up_format_decision_reaches_cloud_init(harness: Harness) -> None:
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


def test_up_reconciles_droplet_only_by_attaching_the_volume(
    harness: Harness,
) -> None:
    unattached = _droplet_state()
    del unattached[0]["volume_ids"]
    harness.write_state("droplet.json", unattached)
    result = harness.run("up", "--yes")
    assert result.code == 0, result.stderr
    attach = _only(harness.doctl_calls(), "compute volume-action attach")
    assert "vol-123" in attach
    assert "9001" in attach
    assert not any("compute droplet create" in call for call in harness.doctl_calls())


def test_up_refuses_non_interactive_without_yes(harness: Harness) -> None:
    result = harness.run("up")
    assert result.code == 2
    assert "non-interactive" in result.stderr
    assert not any("create" in call for call in harness.doctl_calls())
    assert not any("ssh-key import" in call for call in harness.doctl_calls())


def test_up_rejects_duplicate_tags_and_foreign_region(harness: Harness) -> None:
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


def test_up_rejects_volume_below_minimum(harness: Harness) -> None:
    result = harness.run("up", "--yes", "--volume-gb", "5")
    assert result.code == 2
    assert "at least 10" in result.stderr


def test_up_fails_closed_when_size_price_is_missing(harness: Harness) -> None:
    result = harness.run("up", "--yes", env={"FAKE_SIZE_PRICE_NULL": "1"})
    assert result.code == 2
    assert "no monthly price" in result.stderr
    assert not any("create" in call for call in harness.doctl_calls())


def test_up_cloud_init_timeout_prints_logs_and_deletes_nothing(
    harness: Harness,
) -> None:
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


def test_up_reports_a_provisioning_refusal_not_a_transport_failure(
    harness: Harness,
) -> None:
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


def test_up_reports_ssh_255_as_a_transport_failure(harness: Harness) -> None:
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


def test_status_prints_fingerprints_and_fails_closed_when_probe_fails(
    harness: Harness,
) -> None:
    harness.existing()
    harness.materialize_identity()
    ok = harness.run("status")
    assert ok.code == 0, ok.stderr
    assert "SHA256:FAKEKEY" in ok.stdout
    assert "203.0.113.10" in ok.stdout

    failed = harness.run("status", env={"FAKE_FINGERPRINT_EXIT": "1"})
    assert failed.code == 2
    assert "could not read authorized-key fingerprints" in failed.stderr


def test_status_without_resources_is_clean_and_does_not_ssh(
    harness: Harness,
) -> None:
    harness.materialize_identity()
    result = harness.run("status")
    assert result.code == 0, result.stderr
    assert "none (tag slipfork-dev-vm)" in result.stdout
    assert harness.ssh_calls() == []


def test_status_malformed_droplet_networks_fails_closed(harness: Harness) -> None:
    harness.materialize_identity()
    malformed = _droplet_state()
    malformed[0]["networks"] = {"v4": "not-a-list"}
    harness.write_state("droplet.json", malformed)
    result = harness.run("status")
    assert result.code == 2
    assert "malformed networks.v4" in result.stderr
    assert "Traceback" not in result.stderr
    assert not any("create" in call for call in harness.doctl_calls())


def test_identity_flag_is_repeatable_and_first_wins(
    harness: Harness, tmp_path: Path
) -> None:
    harness.write_state("droplet.json", _droplet_state())
    first = tmp_path / "first" / "id_ed25519"
    second = tmp_path / "second" / "id_ed25519"
    do_vm_mod.load_identity(first, materialize=True)
    do_vm_mod.load_identity(second, materialize=True)
    result = harness.run("status", "--identity", str(first), "--identity", str(second))
    assert result.code == 0, result.stderr
    calls = harness.ssh_calls()
    assert any(f"-i {first}" in call for call in calls)
    assert not any(f"-i {second}" in call for call in calls)


def test_sync_and_down_reject_identity_flag(harness: Harness) -> None:
    for command in ("sync", "down"):
        result = harness.run(command, "--identity", "a")
        assert result.code == 2, command
        assert "--identity" in result.stderr


def test_test_names_missing_suites_and_nodes(harness: Harness) -> None:
    result = harness.run("test")
    assert result.code == 2
    for needle in (
        "crates/slipfork-engine/tests/critical_btrfs_live.rs",
        "crates/slipfork/tests/critical_btrfs_live.rs",
        "M3.14",
        "M3.15",
    ):
        assert needle in result.stderr
    assert harness.ssh_calls() == []


def test_test_streams_every_suite_serialized_and_stops_at_first_failure(
    harness: Harness,
) -> None:
    harness.suite_files()
    harness.write_state("droplet.json", _droplet_state())
    harness.materialize_identity()
    failed = harness.run("test", env={"FAKE_ENGINE_EXIT": "101"})
    assert failed.code == 101
    suites = _lines(harness.state / "suites.log")
    assert len(suites) == 1
    assert "cargo test -p slipfork-engine --test critical_btrfs_live" in suites[0]
    assert "-- --test-threads=1" in suites[0]
    assert "--nocapture" not in suites[0]
    assert "HOME=/var/lib/slipfork/home" in suites[0]
    assert "TMPDIR=/var/lib/slipfork/home/tmp" in suites[0]
    assert "runuser -u slipfork" in suites[0]
    assert any("docker info" in call for call in harness.ssh_calls())

    (harness.state / "suites.log").unlink()
    ok = harness.run("test")
    assert ok.code == 0, ok.stderr
    suites = _lines(harness.state / "suites.log")
    assert len(suites) == len(do_vm_mod.SUITES)
    assert [entry[1] for entry in do_vm_mod.SUITES] == [
        "critical_btrfs_live",
        "critical_btrfs_live",
        "critical_cli",
        "critical_slug",
        "critical_ospp_daemon",
    ]
    for command, (package, test, _, _) in zip(suites, do_vm_mod.SUITES, strict=True):
        assert f"cargo test -p {package} --test {test} -- --test-threads=1" in command


def test_test_fails_closed_when_remote_suite_is_missing(harness: Harness) -> None:
    harness.suite_files()
    harness.write_state("droplet.json", _droplet_state())
    harness.materialize_identity()
    result = harness.run("test", env={"FAKE_REMOTE_SUITE_EXIT": "1"})
    assert result.code == 2
    assert "run `tools/do_vm.py sync` first" in result.stderr
    assert not (harness.state / "suites.log").exists()


def test_test_fails_closed_when_cargo_is_too_old(harness: Harness) -> None:
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


def test_sync_refuses_env_files_before_any_ssh(harness: Harness) -> None:
    harness.write_state("droplet.json", _droplet_state())
    harness.materialize_identity()
    (harness.root / ".env").write_text("SECRET=1\n", encoding="utf-8")
    result = harness.run("sync")
    assert result.code == 2
    assert "refusing to sync secret environment file(s)" in result.stderr
    assert harness.ssh_calls() == []


def test_sync_tar_pipes_working_tree_into_staging(harness: Harness) -> None:
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
        "chown -R slipfork:slipfork /home/slipfork/slipfork" in call for call in calls
    )
    extracted = harness.state / "remote"
    assert (extracted / "README.md").read_text(encoding="utf-8") == "hello\n"
    assert (extracted / ".env.example").is_file()
    assert "synced 2 file(s)" in result.stdout


def test_down_keeps_volume_by_default_and_requires_yes(harness: Harness) -> None:
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


def test_down_delete_volume_destroys_droplet_then_volume(harness: Harness) -> None:
    harness.existing()
    confirmed = harness.run("down", "--yes", "--delete-volume")
    assert confirmed.code == 0, confirmed.stderr
    calls = harness.doctl_calls()
    droplet_index = next(
        index for index, call in enumerate(calls) if "compute droplet delete" in call
    )
    volume_index = next(
        index for index, call in enumerate(calls) if "compute volume delete" in call
    )
    assert droplet_index < volume_index
    assert "9001" in calls[droplet_index]
    assert "vol-123" in calls[volume_index]
    assert not (harness.state / "volume.json").exists()
    assert "destroyed 1 droplet(s) and 1 volume(s)" in confirmed.stdout


def test_down_reports_survivors(harness: Harness) -> None:
    harness.write_state("droplet.json", _droplet_state())
    result = harness.run("down", "--yes", env={"FAKE_DELETE_STUCK": "1"})
    assert result.code == 2
    assert "survived deletion" in result.stderr
    assert "droplet 9001" in result.stderr


def test_ssh_uses_identity_and_owner_envelope(
    harness: Harness, monkeypatch: pytest.MonkeyPatch
) -> None:
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


def test_ssh_forwards_remote_command(
    harness: Harness, monkeypatch: pytest.MonkeyPatch
) -> None:
    harness.write_state("droplet.json", _droplet_state())
    harness.materialize_identity()
    recorded: list[list[str]] = []
    monkeypatch.setattr(
        do_vm_mod.os, "execvp", lambda _file, argv: recorded.append(list(argv))
    )
    result = harness.run("ssh", "--", "findmnt", "-no", "OPTIONS", "/var/lib/slipfork")
    assert result.code == 0, result.stderr
    argv = recorded[0]
    assert "-t" not in argv
    remote = argv[-1]
    assert "findmnt -no OPTIONS /var/lib/slipfork" in remote
    assert "command -v slipfork" not in remote
    assert "exec bash" not in remote
    assert "HOME=/var/lib/slipfork/root-home" in remote


def test_no_shell_true_anywhere(
    harness: Harness, monkeypatch: pytest.MonkeyPatch
) -> None:
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


def test_provision_script_is_valid_bash() -> None:
    for renderer in (
        do_vm_mod.render_provision_script,
        do_vm_mod.render_volume_discovery,
        do_vm_mod.render_automount_teardown,
        do_vm_mod.render_volume_signature_step,
        do_vm_mod.render_install_authorized_keys,
    ):
        result = subprocess.run(
            ["bash", "-n"],
            input=renderer(),
            text=True,
            capture_output=True,
            check=False,
        )
        assert result.returncode == 0, result.stderr


def test_volume_discovery_selects_the_disk_and_ignores_partitions(
    tmp_path: Path,
) -> None:
    volumes = tmp_path / "volumes"
    volumes.mkdir()
    disk = volumes / "scsi-0DO_Volume_slipfork-dev-vm"
    disk.write_text("", encoding="utf-8")
    (volumes / "scsi-0DO_Volume_slipfork-dev-vm-part1").write_text("", encoding="utf-8")
    (volumes / "scsi-0DO_Volume_slipfork-dev-vm-part9").write_text("", encoding="utf-8")
    config = do_vm_mod.ProvisionConfig(
        volume_glob=str(volumes / "scsi-0DO_Volume_*"),
        device_wait_attempts=1,
        device_wait_seconds=0,
    )
    script = (
        do_vm_mod.render_volume_discovery(config) + '\nprintf "%s" "$volume_device"\n'
    )
    selected = run_bash(script, tmp_path / "bin")
    assert selected.returncode == 0, selected.stderr
    assert selected.stdout == str(disk)

    (volumes / "scsi-0DO_Volume_other").write_text("", encoding="utf-8")
    ambiguous = run_bash(script, tmp_path / "bin")
    assert ambiguous.returncode == 1
    assert "exactly one attached DO volume disk" in ambiguous.stderr
    assert "scsi-0DO_Volume_other" in ambiguous.stderr


def test_cloud_init_embeds_provision_script_verbatim() -> None:
    script = do_vm_mod.render_provision_script()
    cloud = do_vm_mod.cloud_init()
    dedented = "\n".join(line[6:] for line in cloud.splitlines()[5:])
    assert dedented.rstrip("\n") == script.rstrip("\n")
    assert "mkfs.btrfs" in cloud
    assert do_vm_mod.MARKER in cloud


def test_cloud_init_carries_the_format_decision() -> None:
    assert "_format_allowed=0" in do_vm_mod.cloud_init()
    assert "_format_allowed=1" in do_vm_mod.cloud_init(
        do_vm_mod.ProvisionConfig(allow_format=True)
    )


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


@pytest.mark.parametrize(
    ("env", "device_bytes", "allow_format", "expected_code", "needles", "formats"),
    [
        ({"FAKE_BLKID_RC": "0", "FAKE_BLKID_TYPE": "btrfs"}, b"", False, 0, [], False),
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
    ],
)
def test_volume_signature_step_branches_on_blkid(
    tmp_path: Path,
    env: dict[str, str],
    device_bytes: bytes,
    allow_format: bool,
    expected_code: int,
    needles: list[str],
    formats: bool,
) -> None:
    device = tmp_path / "device"
    device.write_bytes(device_bytes)
    result = _run_signature_step(tmp_path, device, env, allow_format=allow_format)
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


def test_automount_teardown_is_conditional_and_fails_closed(tmp_path: Path) -> None:
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


def test_apparmor_body_renders_the_ruling_exactly() -> None:
    expected = (
        "abi <abi/4.0>,\n"
        "profile slipfork /{usr/local/bin/slipfork{,d,-world-init},"
        "home/slipfork/slipfork/target/{debug,release}/slipfork{,d,-world-init},"
        "usr/bin/unshare,.slipfork/bin/crun} flags=(unconfined) {\n"
        "  userns,\n"
        "}\n"
    )
    assert do_vm_mod.render_apparmor_body("/home/slipfork/slipfork") == expected
    assert do_vm_mod.render_apparmor_body("/home/slipfork/slipfork/") == expected
    assert len(expected.splitlines()) == 4
    assert "//" not in expected


def _built_slipfork() -> Path:
    """The built `slipfork` binary; build it when absent and fail closed if it cannot be built.

    The AppArmor byte-equivalence contract must run on every `make check`; it never skips just
    because `target/debug/slipfork` was not built by an earlier step.
    """
    override = os.environ.get("SLIPFORK_BIN")
    if override:
        binary = Path(override)
    else:
        metadata = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            check=True,
        )
        target_dir = Path(json.loads(metadata.stdout)["target_directory"])
        binary = target_dir / "debug" / "slipfork"
    if not binary.is_file():
        built = subprocess.run(
            ["cargo", "build", "-q", "-p", "slipfork", "--bin", "slipfork"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        assert built.returncode == 0, (
            f"`cargo build -q -p slipfork` failed; the AppArmor contract cannot run: "
            f"{built.stderr}"
        )
    assert binary.is_file(), (
        f"`slipfork` is not built at {binary}; the AppArmor contract cannot run"
    )
    return binary


def _profile_attachment(header: str) -> tuple[str, str, str]:
    """Split `profile slipfork <attachment> flags=(unconfined) {` into its three parts."""
    prefix = "profile slipfork "
    suffix = " flags=(unconfined) {"
    assert header.startswith(prefix), header
    assert header.endswith(suffix), header
    return prefix, header[len(prefix) : -len(suffix)], suffix


_USERNS_TAIL = ",usr/bin/unshare,.slipfork/bin/crun}"


def _split_attachment(attachment: str) -> str:
    """Return the `<bin_dir>` clause of one leading-slash brace attachment expression."""
    assert attachment.startswith("/{"), attachment
    assert attachment.endswith(_USERNS_TAIL), attachment
    bin_clause = attachment[len("/{") : -len(_USERNS_TAIL)]
    assert bin_clause, attachment
    return bin_clause


def test_cli_rendered_apparmor_profile_matches_the_harness_skeleton() -> None:
    """One renderer owner: the harness copy must match `slipfork daemon unit --only apparmor`.

    Both profiles attach every executable that performs `CLONE_NEWUSER` for a world under one
    leading-slash brace expression. The harness expands only the `<bin_dir>` clause to cover
    `/usr/local/bin` plus the Cargo `target/{debug,release}` binaries; the product names the one
    `<bin_dir>`. The four-line body, the ABI header, the profile flags, and the
    `,usr/bin/unshare,.slipfork/bin/crun}` tail are byte-identical, so neither copy can drift.
    """
    binary = _built_slipfork()
    rendered = subprocess.run(
        [str(binary), "daemon", "unit", "--only", "apparmor"],
        capture_output=True,
        text=True,
        check=True,
    )
    cli_lines = rendered.stdout.splitlines(keepends=True)
    harness_lines = do_vm_mod.render_apparmor_body().splitlines(keepends=True)

    assert len(cli_lines) == 4, rendered.stdout
    assert len(harness_lines) == 4
    assert cli_lines[0] == harness_lines[0] == "abi <abi/4.0>,\n"
    assert cli_lines[2] == harness_lines[2] == "  userns,\n"
    assert cli_lines[3] == harness_lines[3] == "}\n"

    cli_prefix, cli_attachment, cli_suffix = _profile_attachment(
        cli_lines[1].rstrip("\n")
    )
    harness_prefix, harness_attachment, harness_suffix = _profile_attachment(
        harness_lines[1].rstrip("\n")
    )
    assert (cli_prefix, cli_suffix) == (harness_prefix, harness_suffix)

    rel_bin_dir = str(binary.parent).strip("/")
    assert cli_attachment == (
        f"/{{{rel_bin_dir}/slipfork{{,d,-world-init}},usr/bin/unshare,.slipfork/bin/crun}}"
    )
    rel_tree = do_vm_mod.REMOTE_TREE.strip("/")
    assert harness_attachment == (
        "/{usr/local/bin/slipfork{,d,-world-init},"
        f"{rel_tree}/target/{{debug,release}}/slipfork{{,d,-world-init}},"
        "usr/bin/unshare,.slipfork/bin/crun}"
    )

    cli_bin_clause = _split_attachment(cli_attachment)
    harness_bin_clause = _split_attachment(harness_attachment)
    assert cli_bin_clause == f"{rel_bin_dir}/slipfork{{,d,-world-init}}"
    assert harness_bin_clause == (
        "usr/local/bin/slipfork{,d,-world-init},"
        f"{rel_tree}/target/{{debug,release}}/slipfork{{,d,-world-init}}"
    )

    # Byte-equality of the whole profile once only the `<bin_dir>` clause is normalised; the
    # `,usr/bin/unshare,.slipfork/bin/crun}` tail is never rewritten, so it cannot drift.
    assert cli_attachment.endswith(_USERNS_TAIL)
    assert harness_attachment.endswith(_USERNS_TAIL)
    cli_profile = "".join(cli_lines).replace(cli_bin_clause, "<bin_clause>")
    harness_profile = "".join(harness_lines).replace(harness_bin_clause, "<bin_clause>")
    assert cli_profile == harness_profile


def test_provision_script_installs_no_host_crun_and_keeps_git() -> None:
    script = do_vm_mod.render_provision_script()
    assert "crun" not in do_vm_mod.PACKAGES.split()
    install = next(line for line in script.splitlines() if "apt-get install" in line)
    assert "crun" not in install.split()
    assert ".slipfork/bin/crun}" in script
    assert "zfs" not in script.lower()
    assert "docker.io docker-buildx" in script
    assert "btrfs-progs" in script
    assert "git" in script
    assert do_vm_mod.RUSTUP_INIT_SHA256 in script
    assert '_rustup_init="$_rustup_dir/rustup-init"' in script
    assert "user_subvol_rm_allowed,noatime" in script
    assert "9123683e" in script
    assert "_format_allowed=0" in script


def test_install_authorized_keys_targets_both_users() -> None:
    script = do_vm_mod.render_install_authorized_keys()
    assert "for _user in root slipfork" in script
    assert "sort -u" in script
    assert script.count("authorized_keys") >= 3


def test_secrets_and_private_key_never_logged(harness: Harness) -> None:
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
