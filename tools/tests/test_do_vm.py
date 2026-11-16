"""Unit tests for `tools/do_vm.py` driven by fake `doctl`/`ssh` executables on `PATH`.

No test touches the DigitalOcean API or a real VM: every cloud/SSH call lands in a shell fake that
logs argv and keeps its state under `tmp_path`.
"""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

import pytest

TOOLS_DIR = Path(__file__).resolve().parent.parent
TOOL = TOOLS_DIR / "do_vm.py"

sys.path.insert(0, str(TOOLS_DIR))
_SPEC = importlib.util.spec_from_file_location("do_vm_under_test", TOOL)
if _SPEC is None or _SPEC.loader is None:
    raise RuntimeError("cannot load tools/do_vm.py for script tests")
do_vm_mod = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = do_vm_mod
_SPEC.loader.exec_module(do_vm_mod)

FAKE_DOCTL = """\
#!/bin/sh
set -eu
printf '%s\\n' "$*" | tr '\\n' ' ' >> "$FAKE_DOCTL_LOG"
printf '\\n' >> "$FAKE_DOCTL_LOG"
case " $* " in
  *" compute ssh-key list "*)
    cat "$FAKE_STATE_DIR/ssh-keys.json" ;;
  *" compute region list "*)
    printf '[{"slug":"ams3","available":true},{"slug":"lon1","available":true}]\\n' ;;
  *" compute size list "*)
    printf '[{"slug":"s-4vcpu-8gb","vcpus":4,"memory":8192,"disk":160,"price_monthly":48.0}]\\n' ;;
  *" compute droplet list "*)
    if [ -f "$FAKE_STATE_DIR/droplet.json" ]; then
      cat "$FAKE_STATE_DIR/droplet.json"
    else
      printf '[]\\n'
    fi ;;
  *" compute volume list "*)
    if [ -f "$FAKE_STATE_DIR/volume.json" ]; then
      cat "$FAKE_STATE_DIR/volume.json"
    else
      printf '[]\\n'
    fi ;;
  *" compute volume create "*)
    cat > "$FAKE_STATE_DIR/volume.json" <<'JSON'
[{"id":"vol-123","name":"slipfork-dev-vm","region":{"slug":"ams3"},"size_gigabytes":100,"tags":["slipfork-dev-vm"],"droplet_ids":[]}]
JSON
    printf '{}\\n' ;;
  *" compute droplet create "*)
    cat > "$FAKE_STATE_DIR/droplet.json" <<'JSON'
[{"id":9001,"name":"slipfork-dev-vm","region":{"slug":"ams3"},"size":{"slug":"s-4vcpu-8gb"},"volume_ids":["vol-123"],"networks":{"v4":[{"type":"public","ip_address":"203.0.113.10"}]}}]
JSON
    printf '{}\\n' ;;
  *" compute volume-action attach "*)
    cat > "$FAKE_STATE_DIR/droplet.json" <<'JSON'
[{"id":9001,"name":"slipfork-dev-vm","region":{"slug":"ams3"},"size":{"slug":"s-4vcpu-8gb"},"volume_ids":["vol-123"],"networks":{"v4":[{"type":"public","ip_address":"203.0.113.10"}]}}]
JSON
    printf '{}\\n' ;;
  *" compute droplet delete "*)
    rm -f "$FAKE_STATE_DIR/droplet.json"
    printf '{}\\n' ;;
  *" compute volume delete "*)
    rm -f "$FAKE_STATE_DIR/volume.json"
    printf '{}\\n' ;;
  *)
    echo "fake doctl: unhandled argv: $*" >&2
    exit 9 ;;
esac
"""

FAKE_SSH = """\
#!/bin/sh
set -eu
printf '%s\\n' "$*" | tr '\\n' ' ' >> "$FAKE_SSH_LOG"
printf '\\n' >> "$FAKE_SSH_LOG"
case " $* " in
  *"ssh-keygen -lf /root/.ssh/authorized_keys"*)
    printf '256 SHA256:FAKEKEY owner-laptop (ED25519)\\n'
    exit "${FAKE_FINGERPRINT_EXIT:-0}" ;;
  *"test -f /root/slipfork/crates/slipfork-engine/tests/critical_incus_live.rs"*)
    exit "${FAKE_REMOTE_SUITE_EXIT:-1}" ;;
  *" command -v cargo "*)
    exit "${FAKE_CARGO_EXIT:-0}" ;;
  *" test -f /root/incus-ready "*)
    if [ -n "${FAKE_READY_STDERR:-}" ]; then
      printf '%s\\n' "$FAKE_READY_STDERR" >&2
    fi
    exit "${FAKE_READY_EXIT:-0}" ;;
  *"cat >> /root/.ssh/authorized_keys"*)
    mkdir -p "$FAKE_STATE_DIR"
    cat >> "$FAKE_STATE_DIR/authorized_keys"
    sort -u -o "$FAKE_STATE_DIR/authorized_keys" "$FAKE_STATE_DIR/authorized_keys" ;;
  *"tar -x -C /root/slipfork"*)
    cat >/dev/null ;;
  *" bash -s "*)
    cat >/dev/null ;;
esac
exit 0
"""

PUBLIC_KEY = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFAKETESTKEY owner-laptop\n"


@dataclass
class Harness:
    root: Path
    state: Path
    doctl_log: Path
    ssh_log: Path
    env: dict[str, str]

    def run(
        self, *args: str, env: dict[str, str] | None = None
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(TOOL), *args],
            cwd=self.root,
            env={**self.env, **(env or {})},
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=False,
        )

    def doctl_calls(self) -> list[str]:
        return _lines(self.doctl_log)

    def ssh_calls(self) -> list[str]:
        return _lines(self.ssh_log)

    def write_state(self, name: str, value: object) -> None:
        (self.state / name).write_text(json.dumps(value), encoding="utf-8")

    def key_file(self, name: str = "id_ed25519.pub") -> Path:
        path = self.root / name
        path.write_text(PUBLIC_KEY, encoding="utf-8")
        return path


def _lines(path: Path) -> list[str]:
    if not path.is_file():
        return []
    return [line for line in path.read_text(encoding="utf-8").splitlines() if line]


def _write_executable(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(0o755)


def _git_init(root: Path) -> None:
    env = {**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null"}
    subprocess.run(["git", "init", "-b", "develop"], cwd=root, check=True, env=env)
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"],
        cwd=root,
        check=True,
        env=env,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test"], cwd=root, check=True, env=env
    )


@pytest.fixture
def harness(tmp_path: Path) -> Harness:
    root = tmp_path / "repo"
    root.mkdir()
    _git_init(root)
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
                {"id": 101, "name": "bench-key", "fingerprint": "SHA256:one"},
                {"id": 202, "name": "owner-laptop", "fingerprint": "SHA256:two"},
            ]
        ),
        encoding="utf-8",
    )
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:{os.environ['PATH']}",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "FAKE_STATE_DIR": str(state),
        "FAKE_DOCTL_LOG": str(doctl_log),
        "FAKE_SSH_LOG": str(ssh_log),
    }
    return Harness(root, state, doctl_log, ssh_log, env)


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
        }
    ]


def _only(calls: list[str], needle: str) -> str:
    matches = [call for call in calls if needle in call]
    assert len(matches) == 1, f"expected exactly one {needle!r} call, got {matches}"
    return matches[0]


def test_help_lists_exactly_the_six_commands(harness: Harness) -> None:
    result = harness.run("--help")
    assert result.returncode == 0, result.stderr
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


def test_up_help_shows_required_flags(harness: Harness) -> None:
    result = harness.run("up", "--help")
    assert result.returncode == 0, result.stderr
    for flag in (
        "--region",
        "--size",
        "--volume-gb",
        "--yes",
        "--ssh-key",
        "--authorized-key",
    ):
        assert flag in result.stdout


def test_up_defaults_and_argv_construct_ams3_s_vm(harness: Harness) -> None:
    result = harness.run("up", "--yes")
    assert result.returncode == 0, result.stderr
    calls = harness.doctl_calls()

    volume_create = _only(calls, "compute volume create")
    assert "--region ams3" in volume_create
    assert "--size 100GiB" in volume_create
    assert "--tag slipfork-dev-vm" in volume_create

    droplet_create = _only(calls, "compute droplet create")
    assert "--region ams3" in droplet_create
    assert "--size s-4vcpu-8gb" in droplet_create
    assert "--image ubuntu-24-04-x64" in droplet_create
    assert "--volumes vol-123" in droplet_create
    assert "--tag-name slipfork-dev-vm" in droplet_create
    assert "--ssh-keys 101,202" in droplet_create
    assert "set -euo pipefail" in droplet_create
    assert "apt-cache policy incus" in droplet_create
    assert "zpool create -o ashift=12" in droplet_create
    assert "incus storage create default zfs source=incuspool" in droplet_create
    assert "zabbly" not in droplet_create.lower()
    assert "101, 202 (all keys visible to this doctl context)" in result.stdout
    assert "prereq   install Rust >= 1.85" in result.stdout
    assert not any("delete" in call for call in calls)
    assert "$58.00/month" in result.stdout
    assert "ams3" in result.stdout
    assert "s-4vcpu-8gb" in result.stdout
    assert "tools/do_vm.py down --yes" in result.stdout


def test_up_reuses_tagged_droplet_and_volume(harness: Harness) -> None:
    harness.write_state("droplet.json", _droplet_state())
    harness.write_state("volume.json", _volume_state())
    result = harness.run("up", "--yes")
    assert result.returncode == 0, result.stderr
    assert "reusing droplet 9001" in result.stdout
    assert "existing tagged droplet/volume win" in result.stdout
    calls = harness.doctl_calls()
    assert not any("create" in call for call in calls)
    assert not any("volume-action attach" in call for call in calls)
    assert "warning: no --authorized-key" in result.stderr

    again = harness.run("up", "--yes")
    assert again.returncode == 0, again.stderr
    assert not any("create" in call for call in harness.doctl_calls())
    assert not any("volume-action attach" in call for call in harness.doctl_calls())


def test_up_fails_fast_on_publickey_denied(harness: Harness) -> None:
    harness.write_state("droplet.json", _droplet_state())
    harness.write_state("volume.json", _volume_state())
    result = harness.run(
        "up",
        "--yes",
        env={
            "FAKE_READY_EXIT": "255",
            "FAKE_READY_STDERR": "root@203.0.113.10: Permission denied (publickey).",
        },
    )
    assert result.returncode == 2
    assert "SSH authentication" in result.stderr
    assert "--ssh-key" in result.stderr
    assert "--authorized-key" in result.stderr
    assert "cloud-init did not finish" not in result.stderr
    assert not any("delete" in call for call in harness.doctl_calls())


def test_up_prints_summary_before_fingerprints_and_never_deletes(
    harness: Harness,
) -> None:
    harness.write_state("droplet.json", _droplet_state())
    harness.write_state("volume.json", _volume_state())
    ok = harness.run("up", "--yes")
    assert ok.returncode == 0, ok.stderr
    assert ok.stdout.index("down     tools/do_vm.py down --yes") < ok.stdout.index(
        "keys     authorized_keys fingerprints"
    )
    assert not any("delete" in call for call in harness.doctl_calls())

    failed = harness.run("up", "--yes", env={"FAKE_FINGERPRINT_EXIT": "1"})
    assert failed.returncode == 2
    assert "down     tools/do_vm.py down --yes" in failed.stdout
    assert "tools/do_vm.py down --yes" in failed.stderr
    assert "could not read authorized-key fingerprints" in failed.stderr
    assert not any("delete" in call for call in harness.doctl_calls())


def test_status_malformed_droplet_networks_fails_closed(harness: Harness) -> None:
    malformed = _droplet_state()
    malformed[0]["networks"] = {"v4": "not-a-list"}
    harness.write_state("droplet.json", malformed)
    result = harness.run("status")
    assert result.returncode == 2
    assert "malformed networks.v4" in result.stderr
    assert "Traceback" not in result.stderr


def test_ssh_command_allows_interactive_auth(harness: Harness) -> None:
    harness.write_state("droplet.json", _droplet_state())
    result = harness.run("ssh")
    assert result.returncode == 0, result.stderr
    call = _only(harness.ssh_calls(), "root@203.0.113.10")
    assert "BatchMode=yes" not in call


def test_provision_script_is_valid_bash() -> None:
    result = subprocess.run(
        ["bash", "-n"],
        input=do_vm_mod.PROVISION_SCRIPT,
        text=True,
        capture_output=True,
        check=False,
    )
    assert result.returncode == 0, result.stderr


def test_provision_script_fails_closed_when_incus_candidate_is_none(
    tmp_path: Path,
) -> None:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    forbidden_log = tmp_path / "forbidden.log"
    _write_executable(
        bin_dir / "apt-cache",
        "#!/bin/sh\nprintf 'Package: incus\\nCandidate: (none)\\n'\n",
    )
    _write_executable(bin_dir / "apt-get", "#!/bin/sh\nexit 0\n")
    for command in ("systemctl", "zpool", "incus", "journalctl"):
        _write_executable(
            bin_dir / command,
            f'#!/bin/sh\nprintf "%s\\n" "{command}" >> "$FAKE_FORBIDDEN_LOG"\nexit 99\n',
        )
    result = subprocess.run(
        ["bash"],
        input=do_vm_mod.PROVISION_SCRIPT,
        text=True,
        capture_output=True,
        check=False,
        env={
            **os.environ,
            "PATH": f"{bin_dir}:{os.environ['PATH']}",
            "FAKE_FORBIDDEN_LOG": str(forbidden_log),
        },
    )
    assert result.returncode == 1
    assert "Candidate: (none)" in result.stderr
    assert "unavailable in this image" in result.stderr
    assert not forbidden_log.exists() or forbidden_log.read_text() == ""


def test_up_reconciles_droplet_only_by_creating_and_attaching_the_volume(
    harness: Harness,
) -> None:
    unattached = _droplet_state()
    del unattached[0]["volume_ids"]
    harness.write_state("droplet.json", unattached)
    result = harness.run("up", "--yes")
    assert result.returncode == 0, result.stderr
    calls = harness.doctl_calls()
    assert "volume   create 100 GiB" in result.stdout
    _only(calls, "compute volume create")
    attach = _only(calls, "compute volume-action attach")
    assert "vol-123" in attach
    assert "9001" in attach
    assert not any("compute droplet create" in call for call in calls)

    harness.doctl_log.unlink()
    again = harness.run("up", "--yes")
    assert again.returncode == 0, again.stderr
    assert not any("create" in call for call in harness.doctl_calls())
    assert not any("volume-action attach" in call for call in harness.doctl_calls())


def test_up_reconciles_volume_only_by_creating_the_droplet_with_it(
    harness: Harness,
) -> None:
    harness.write_state("volume.json", _volume_state())
    result = harness.run("up", "--yes")
    assert result.returncode == 0, result.stderr
    calls = harness.doctl_calls()
    assert "volume   reuse vol-123 (100 GiB)" in result.stdout
    droplet_create = _only(calls, "compute droplet create")
    assert "--volumes vol-123" in droplet_create
    assert not any("compute volume create" in call for call in calls)
    assert not any("volume-action attach" in call for call in calls)

    harness.doctl_log.unlink()
    again = harness.run("up", "--yes")
    assert again.returncode == 0, again.stderr
    assert not any("create" in call for call in harness.doctl_calls())
    assert not any("volume-action attach" in call for call in harness.doctl_calls())


def test_up_fails_closed_with_zero_visible_keys(harness: Harness) -> None:
    (harness.state / "ssh-keys.json").write_text("[]", encoding="utf-8")
    result = harness.run("up", "--yes")
    assert result.returncode == 2
    assert "reports no SSH keys" in result.stderr
    assert not any("create" in call for call in harness.doctl_calls())


def test_ssh_key_restriction_resolves_name_and_id(harness: Harness) -> None:
    by_name = harness.run("up", "--yes", "--ssh-key", "owner-laptop")
    assert by_name.returncode == 0, by_name.stderr
    assert "--ssh-keys 202" in _only(harness.doctl_calls(), "compute droplet create")
    assert "(restricted by --ssh-key)" in by_name.stdout

    harness.write_state("droplet.json", [])
    harness.write_state("volume.json", [])
    harness.doctl_log.unlink()
    by_id = harness.run("up", "--yes", "--ssh-key", "101")
    assert by_id.returncode == 0, by_id.stderr
    assert "--ssh-keys 101" in _only(harness.doctl_calls(), "compute droplet create")


def test_ssh_key_unknown_fails_closed(harness: Harness) -> None:
    result = harness.run("up", "--yes", "--ssh-key", "nope")
    assert result.returncode == 2
    assert "matches no key visible" in result.stderr
    assert not any("compute droplet create" in call for call in harness.doctl_calls())


def test_authorized_key_is_appended_idempotently_and_fingerprints_printed(
    harness: Harness,
) -> None:
    key = harness.key_file()
    first = harness.run("up", "--yes", "--authorized-key", str(key))
    assert first.returncode == 0, first.stderr
    authorized = harness.state / "authorized_keys"
    assert authorized.read_text(encoding="utf-8") == PUBLIC_KEY
    assert "SHA256:FAKEKEY owner-laptop" in first.stdout

    second = harness.run("up", "--yes", "--authorized-key", str(key))
    assert second.returncode == 0, second.stderr
    assert authorized.read_text(encoding="utf-8") == PUBLIC_KEY
    assert authorized.read_text(encoding="utf-8").count("owner-laptop") == 1
    installs = [
        call
        for call in harness.ssh_calls()
        if "cat >> /root/.ssh/authorized_keys" in call
    ]
    assert len(installs) == 2
    assert PUBLIC_KEY.strip() not in "\n".join(harness.ssh_calls())


def test_status_prints_fingerprints_and_fails_closed_when_probe_fails(
    harness: Harness,
) -> None:
    harness.write_state("droplet.json", _droplet_state())
    harness.write_state("volume.json", _volume_state())
    ok = harness.run("status")
    assert ok.returncode == 0, ok.stderr
    assert "SHA256:FAKEKEY owner-laptop" in ok.stdout
    assert "203.0.113.10" in ok.stdout

    failed = harness.run("status", env={"FAKE_FINGERPRINT_EXIT": "1"})
    assert failed.returncode == 2
    assert "could not read authorized-key fingerprints" in failed.stderr
    assert "203.0.113.10" in failed.stderr


def test_status_without_resources_is_clean_and_does_not_ssh(harness: Harness) -> None:
    result = harness.run("status")
    assert result.returncode == 0, result.stderr
    assert "none (tag slipfork-dev-vm)" in result.stdout
    assert harness.ssh_calls() == []


def test_test_command_names_missing_suite_and_m3_3(harness: Harness) -> None:
    result = harness.run("test")
    assert result.returncode == 2
    assert "critical_incus_live" in result.stderr
    assert "M3.3" in result.stderr
    assert harness.ssh_calls() == []


def test_test_streams_suite_and_forwards_extra_cargo_args(harness: Harness) -> None:
    suite = harness.root / "crates/slipfork-engine/tests/critical_incus_live.rs"
    suite.parent.mkdir(parents=True)
    suite.write_text("#[test]\nfn live() {}\n", encoding="utf-8")
    harness.write_state("droplet.json", _droplet_state())
    result = harness.run(
        "test",
        "--",
        "--nocapture",
        env={"FAKE_REMOTE_SUITE_EXIT": "0"},
    )
    assert result.returncode == 0, result.stderr
    cargo = _only(harness.ssh_calls(), "cargo test -p slipfork-engine")
    assert "--test critical_incus_live" in cargo
    assert "-- --test-threads=1 --nocapture" in cargo


def test_sync_refuses_env_files_before_any_ssh(harness: Harness) -> None:
    harness.write_state("droplet.json", _droplet_state())
    harness.write_state("volume.json", _volume_state())
    (harness.root / ".env").write_text("SECRET=1\n", encoding="utf-8")
    result = harness.run("sync")
    assert result.returncode == 2
    assert "refusing to sync secret environment file(s)" in result.stderr
    assert ".env" in result.stderr
    assert harness.ssh_calls() == []


def test_sync_tar_pipes_working_tree_and_allows_env_example(harness: Harness) -> None:
    harness.write_state("droplet.json", _droplet_state())
    (harness.root / ".env.example").write_text("SECRET=\n", encoding="utf-8")
    (harness.root / "README.md").write_text("hello\n", encoding="utf-8")
    result = harness.run("sync")
    assert result.returncode == 0, result.stderr
    calls = harness.ssh_calls()
    assert any("mkdir -p /root/slipfork" in call for call in calls)
    assert any("tar -x -C /root/slipfork" in call for call in calls)
    assert "synced 2 file(s)" in result.stdout


def test_sync_does_not_refuse_env_files_excluded_from_the_tar(
    harness: Harness,
) -> None:
    harness.write_state("droplet.json", _droplet_state())
    (harness.root / ".gitignore").write_text("vendor/\n", encoding="utf-8")
    subprocess.run(
        ["git", "add", ".gitignore"],
        cwd=harness.root,
        check=True,
        env=harness.env,
    )
    vendor = harness.root / "vendor"
    vendor.mkdir()
    (vendor / ".env").write_text("SECRET=1\n", encoding="utf-8")
    (harness.root / "README.md").write_text("hello\n", encoding="utf-8")
    result = harness.run("sync")
    assert result.returncode == 0, result.stderr
    assert "refusing to sync secret" not in result.stderr
    assert "synced 2 file(s)" in result.stdout


def test_down_deletes_droplet_before_volume_and_never_without_yes(
    harness: Harness,
) -> None:
    harness.write_state("droplet.json", _droplet_state())
    harness.write_state("volume.json", _volume_state())
    refused = harness.run("down")
    assert refused.returncode == 2
    assert "non-interactive" in refused.stderr
    assert not any("delete" in call for call in harness.doctl_calls())

    confirmed = harness.run("down", "--yes")
    assert confirmed.returncode == 0, confirmed.stderr
    calls = harness.doctl_calls()
    droplet_index = next(
        index for index, call in enumerate(calls) if "compute droplet delete" in call
    )
    volume_index = next(
        index for index, call in enumerate(calls) if "compute volume delete" in call
    )
    assert droplet_index < volume_index
    assert "--force" in calls[droplet_index]
    assert "--force" in calls[volume_index]
    assert "9001" in calls[droplet_index]
    assert "vol-123" in calls[volume_index]
    assert "destroyed 1 droplet(s) and 1 volume(s)" in confirmed.stdout


def test_down_succeeds_with_only_a_droplet(harness: Harness) -> None:
    harness.write_state("droplet.json", _droplet_state())
    result = harness.run("down", "--yes")
    assert result.returncode == 0, result.stderr
    calls = harness.doctl_calls()
    assert len([call for call in calls if "compute droplet delete" in call]) == 1
    assert not any("compute volume delete" in call for call in calls)
    assert "destroyed 1 droplet(s) and 0 volume(s)" in result.stdout


def test_down_succeeds_with_only_a_volume(harness: Harness) -> None:
    harness.write_state("volume.json", _volume_state())
    result = harness.run("down", "--yes")
    assert result.returncode == 0, result.stderr
    calls = harness.doctl_calls()
    assert len([call for call in calls if "compute volume delete" in call]) == 1
    assert not any("compute droplet delete" in call for call in calls)
    assert "destroyed 0 droplet(s) and 1 volume(s)" in result.stdout
