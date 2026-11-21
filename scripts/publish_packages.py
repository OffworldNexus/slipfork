#!/usr/bin/env python3
"""Publish Slipfork packages to Cloudsmith (primary) and Forgejo (mirror).

Every mode honours ``--dry-run``: it prints the exact HTTP request and touches no
network. A missing ``CLOUDSMITH_API_KEY`` fails closed (exit 1); a missing
``FORGEJO_TOKEN`` is a notice and exit 0, because the mirror is best effort.

Retention counts *runs*, not packages: one nightly produces a deb, rpm, arch and
apk with one version string, so ``prune`` deletes whole ``(date, run)`` groups and
never splits an architecture pair.
"""

from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORG = "offworld-nexus"
REPO = "slipfork"
CLOUDSMITH_API = f"https://api.cloudsmith.io/v1/packages/{ORG}/{REPO}/"
CLOUDSMITH_UPLOAD = f"https://upload.cloudsmith.io/{ORG}/{REPO}/"
FORGEJO_API = f"https://forge.offworld.nexus/api/packages/{ORG}/generic/{REPO}"
BUILDERS = {
    "deb": "build-deb.sh",
    "rpm": "build-rpm.sh",
    "arch": "build-arch.sh",
    "apk": "build-apk.sh",
}
NIGHTLY_RE = re.compile(r"-0\.nightly\.(\d{8})\.r(\d+)\.")


def workspace_version(root: Path = ROOT) -> str:
    """The single version the workspace declares in `[workspace.package]`."""
    text = (root / "Cargo.toml").read_text(encoding="utf-8")
    section = text.split("[workspace.package]", 1)[1]
    match = re.search(r'^version\s*=\s*"([^"]+)"', section, re.MULTILINE)
    if not match:
        raise SystemExit(
            "error: Cargo.toml has no [workspace.package] version; next: restore it"
        )
    return match.group(1)


def nightly_version(base: str, date: str, run: int, sha: str) -> str:
    """`0.1.0-0.nightly.20261121.r42.gabc1234` — the `-0.` sorts below a release."""
    if not re.fullmatch(r"\d{8}", date):
        raise SystemExit(f"error: {date} is not YYYYMMDD; next: pass --date 20261121")
    if run < 1:
        raise SystemExit(f"error: run {run} is not positive; next: pass --run <n>")
    if not re.fullmatch(r"[0-9a-fA-F]{7,40}", sha):
        raise SystemExit(f"error: {sha!r} is not a git sha; next: pass --sha <7+ hex>")
    return f"{base}-0.nightly.{date}.r{run}.g{sha[:7].lower()}"


def parse_nightly(version: str):
    """`(date, run)` for a nightly version, or None for a release."""
    match = NIGHTLY_RE.search(version)
    if not match:
        return None
    return match.group(1), int(match.group(2))


def retention_plan(versions, keep: int = 7):
    """Split versions into (keep, drop) by whole `(date, run)` groups, newest first."""
    if keep < 1:
        raise SystemExit(f"error: --keep {keep} is not positive; next: pass --keep 7")
    groups: dict[tuple[str, int], list[str]] = {}
    for version in versions:
        key = parse_nightly(version)
        if key is None:
            continue
        groups.setdefault(key, []).append(version)
    ordered = sorted(groups, reverse=True)
    kept, dropped = [], []
    for index, key in enumerate(ordered):
        (kept if index < keep else dropped).extend(groups[key])
    return kept, dropped


def _token(env: str) -> str:
    return os.environ.get(env, "").strip()


def _print_request(method: str, url: str, headers, body=None, files=None):
    print(f"{method} {url}")
    for key, value in headers.items():
        print(f"  {key}: {value}")
    if files:
        for name, path in files.items():
            print(f"  file: {name}=<{path}>")
    if body is not None:
        print(json.dumps(body, indent=2))


def cloudsmith_upload(path: str, component: str, dry_run: bool) -> int:
    token = _token("CLOUDSMITH_API_KEY")
    if not token and not dry_run:
        print(
            "error: CLOUDSMITH_API_KEY is unset, so the primary registry is unavailable",
            file=sys.stderr,
        )
        print(
            "next: export CLOUDSMITH_API_KEY=<token>, or re-run with --dry-run",
            file=sys.stderr,
        )
        return 1
    name = Path(path).name
    url = f"{CLOUDSMITH_UPLOAD}?component={component}&name={name}"
    headers = {"X-Api-Key": "<redacted>", "Accept": "application/json"}
    if dry_run:
        _print_request("POST", url, headers, files={"package_file": path})
        return 0
    boundary = "----slipfork-boundary"
    payload = Path(path).read_bytes()
    body = (
        (
            f'--{boundary}\r\nContent-Disposition: form-data; name="package_file"; '
            f'filename="{name}"\r\nContent-Type: application/octet-stream\r\n\r\n'
        ).encode()
        + payload
        + f"\r\n--{boundary}--\r\n".encode()
    )
    request = urllib.request.Request(url, data=body, method="POST")
    request.add_header("X-Api-Key", token)
    request.add_header("Content-Type", f"multipart/form-data; boundary={boundary}")
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            print(f"cloudsmith: {response.status} {name} -> {component}")
    except urllib.error.HTTPError as error:
        print(
            f"error: Cloudsmith rejected {name}: {error.code} {error.reason}",
            file=sys.stderr,
        )
        return 1
    return 0


def forgejo_upload(path: str, component: str, dry_run: bool) -> int:
    token = _token("FORGEJO_TOKEN")
    name = Path(path).name
    url = f"{FORGEJO_API}/{component}/{name}"
    headers = {
        "Authorization": "token <redacted>",
        "Content-Type": "application/octet-stream",
    }
    if not token:
        print(f"notice: FORGEJO_TOKEN is unset; skipped the mirror upload of {name}")
        return 0
    if dry_run:
        _print_request("PUT", url, headers, files={"body": path})
        return 0
    request = urllib.request.Request(url, data=Path(path).read_bytes(), method="PUT")
    request.add_header("Authorization", f"token {token}")
    request.add_header("Content-Type", "application/octet-stream")
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            print(f"forgejo: {response.status} {name} -> {component}")
    except urllib.error.URLError as error:
        # The mirror is best effort: DNS failures and timeouts are warnings too.
        reason = getattr(error, "code", error.reason)
        print(f"warning: Forgejo did not accept {name}: {reason}", file=sys.stderr)
    return 0


def cloudsmith_versions(component: str, token: str, fmt: str):
    versions = []
    page = 1
    while True:
        url = f"{CLOUDSMITH_API}?query=component:{component}%20format:{fmt}&page={page}&page_size=100"
        request = urllib.request.Request(url)
        request.add_header("X-Api-Key", token)
        with urllib.request.urlopen(request, timeout=60) as response:
            batch = json.load(response)
        if not batch:
            return versions
        for package in batch:
            version = package.get("version", "")
            slug = package.get("slug_perm") or package.get("slug")
            if version and slug:
                versions.append((version, slug, package.get("format", "")))
        page += 1


def prune(component: str, keep: int, dry_run: bool, fmt: str) -> int:
    token = _token("CLOUDSMITH_API_KEY")
    if not token and not dry_run:
        print(
            "error: CLOUDSMITH_API_KEY is unset, so nothing can be pruned",
            file=sys.stderr,
        )
        print(
            "next: export CLOUDSMITH_API_KEY=<token>, or re-run with --dry-run",
            file=sys.stderr,
        )
        return 1
    if dry_run:
        print(f"GET {CLOUDSMITH_API}?query=component:{component}%20format:{fmt}&page=1&page_size=100")
        print(
            f"  plan: keep the newest {keep} (date, run) groups; DELETE older groups whole"
        )
        return 0
    packages = [
        entry
        for entry in cloudsmith_versions(component, token, fmt)
        if not entry[2] or entry[2] == fmt
    ]
    kept, dropped = retention_plan([version for version, _, _ in packages], keep)
    print(f"prune: keep {len(kept)} {fmt} packages, drop {len(dropped)}")
    # One nightly version owns several archives (arch pair, signature); map a
    # version to every slug so a dropped group never loses half its architectures.
    slugs: dict[str, list[str]] = {}
    for version, slug, _ in packages:
        slugs.setdefault(version, []).append(slug)
    for version in dropped:
        for slug in slugs[version]:
            url = f"{CLOUDSMITH_API}{slug}/"
            request = urllib.request.Request(url, method="DELETE")
            request.add_header("X-Api-Key", token)
            try:
                urllib.request.urlopen(request, timeout=60).close()
                print(f"deleted {version} ({slug})")
            except urllib.error.URLError as error:
                reason = getattr(error, "code", error.reason)
                print(f"warning: could not delete {version}: {reason}", file=sys.stderr)
    return 0


def stamp(args) -> int:
    base = args.base or workspace_version()
    version = nightly_version(base, args.date, args.run, args.sha)
    script = BUILDERS[args.format]
    command = [
        str(ROOT / "scripts" / script),
        "--version",
        version,
        "--bundle",
        args.bundle,
        "--out",
        args.out,
    ]
    if args.dry_run:
        print(" ".join(command))
        print(f"version: {version}")
        return 0
    print(f"stamp: rebuilt {args.format} at {version}")
    return subprocess.call(command)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="publish_packages.py", description=__doc__)
    sub = parser.add_subparsers(dest="mode", required=True)
    base = sub.add_parser("base-version", help="print the workspace version")
    base.set_defaults(func=lambda _args: print(workspace_version()) or 0)

    release = sub.add_parser("release-stamp", help="print a nightly version string")
    release.add_argument("--base")
    release.add_argument("--date", default=datetime.date.today().strftime("%Y%m%d"))
    release.add_argument(
        "--run", type=int, default=int(os.environ.get("GITHUB_RUN_NUMBER", "1"))
    )
    release.add_argument("--sha", default=os.environ.get("GITHUB_SHA", "0" * 7))
    release.set_defaults(
        func=lambda args: (
            print(
                nightly_version(
                    args.base or workspace_version(), args.date, args.run, args.sha
                )
            )
            or 0
        )
    )

    st = sub.add_parser("stamp", help="rebuild one package format at the nightly stamp")
    st.add_argument("--format", choices=sorted(BUILDERS), required=True)
    st.add_argument("--base")
    st.add_argument("--date", default=datetime.date.today().strftime("%Y%m%d"))
    st.add_argument(
        "--run", type=int, default=int(os.environ.get("GITHUB_RUN_NUMBER", "1"))
    )
    st.add_argument("--sha", default=os.environ.get("GITHUB_SHA", "0" * 7))
    st.add_argument("--bundle", default="target/package-bundle")
    st.add_argument("--out", default="dist/packages")
    st.add_argument("--dry-run", action="store_true")
    st.set_defaults(func=stamp)

    push = sub.add_parser("push", help="upload one artifact to Cloudsmith")
    push.add_argument("--format", choices=sorted(BUILDERS), required=True)
    push.add_argument("--file", required=True)
    push.add_argument("--component", default="nightly")
    push.add_argument("--dry-run", action="store_true")
    push.set_defaults(
        func=lambda args: cloudsmith_upload(args.file, args.component, args.dry_run)
    )

    mirror = sub.add_parser(
        "forgejo", help="mirror one artifact to Forgejo (best effort)"
    )
    mirror.add_argument("--format", choices=sorted(BUILDERS), required=True)
    mirror.add_argument("--file", required=True)
    mirror.add_argument("--component", default="nightly")
    mirror.add_argument("--dry-run", action="store_true")
    mirror.set_defaults(
        func=lambda args: forgejo_upload(args.file, args.component, args.dry_run)
    )

    pr = sub.add_parser("prune", help="delete whole nightly (date, run) groups")
    pr.add_argument("--format", choices=sorted(BUILDERS), required=True)
    pr.add_argument("--component", default="nightly")
    pr.add_argument("--keep", type=int, default=7)
    pr.add_argument("--dry-run", action="store_true")
    pr.set_defaults(func=lambda args: prune(args.component, args.keep, args.dry_run, args.format))
    return parser


def main(argv=None) -> int:
    args = build_parser().parse_args(argv)
    return args.func(args) or 0


if __name__ == "__main__":
    sys.exit(main())
