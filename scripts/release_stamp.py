#!/usr/bin/env python3
"""Single source of truth for Slipfork's per-ecosystem version stamps.

Every format sorts a nightly strictly below the ``X.Y.Z`` release it leads up
to, and monotonically across runs on the same day through ``GITHUB_RUN_NUMBER``:

===================  ==========================================================
Debian               ``<base>~nightly<YYYYMMDD>.r<run>+g<sha7>``
RPM                  ``Version: <base>`` ``Release: 0.nightly.<YYYYMMDD>.r<run>.g<sha7>``
Alpine APK           ``pkgver=<base>_alpha<YYYYMMDD><run>`` ``pkgrel=0``
Arch Linux           ``pkgver=<base>r<run>.g<sha7>`` ``pkgrel=1``
===================  ==========================================================

The Makefile calls ``env`` and passes each value to the matching builder, so no
bash script re-implements the grammar. ``self-test`` pins the exact strings and
is what ``make packages-test`` and CI run; run it with ``uv run --no-project``.
"""

from __future__ import annotations

import argparse
import re
import sys

BASE_RE = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")
DATE_RE = re.compile(r"^\d{8}$")
SHA_RE = re.compile(r"^[0-9a-f]{7,40}$")
CHANNELS = ("stable", "nightly")


class StampError(ValueError):
    """Raised when a version stamp would violate a repository's ordering."""


def stamps(
    *,
    base: str,
    channel: str,
    date: str = "",
    run: int = 0,
    sha: str = "",
) -> dict[str, str]:
    """Return every ecosystem's version field for ``base`` on ``channel``."""
    if not BASE_RE.match(base):
        raise StampError(f"invalid base version {base!r}; next: pass --base 0.2.0")
    if channel not in CHANNELS:
        raise StampError(
            f"invalid channel {channel!r}; next: pass --channel stable|nightly"
        )
    if channel == "stable":
        return {
            "version": base,
            "deb": base,
            "rpm_version": base,
            "rpm_release": "1",
            "apk_version": base,
            "apk_pkgrel": "0",
            "arch_version": base,
            "arch_pkgrel": "1",
        }

    if not DATE_RE.match(date):
        raise StampError(
            f"invalid nightly date {date!r}; next: pass --date $(date -u +%Y%m%d)"
        )
    if run < 1:
        raise StampError(
            f"invalid nightly run {run!r}; next: pass STAMP_RUN=<GITHUB_RUN_NUMBER>"
        )
    if not SHA_RE.match(sha):
        raise StampError(
            f"invalid sha {sha!r}; next: pass STAMP_SHA=$(git rev-parse --short=7 HEAD)"
        )
    short = sha[:7]
    return {
        "version": f"{base}~nightly{date}.r{run}+g{short}",
        "deb": f"{base}~nightly{date}.r{run}+g{short}",
        "rpm_version": base,
        "rpm_release": f"0.nightly.{date}.r{run}.g{short}",
        # Alpine rejects `~` and `+` in pkgver; `_alpha` sorts below <base>-r0.
        "apk_version": f"{base}_alpha{date}{run}",
        "apk_pkgrel": "0",
        "arch_version": f"{base}r{run}.g{short}",
        "arch_pkgrel": "1",
    }


def _shell_quote(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


def _env_names(stamp: dict[str, str]) -> list[tuple[str, str]]:
    return [
        ("VERSION_STAMP", stamp["version"]),
        ("DEB_VERSION", stamp["deb"]),
        ("RPM_VERSION", stamp["rpm_version"]),
        ("RPM_RELEASE", stamp["rpm_release"]),
        ("APK_VERSION", stamp["apk_version"]),
        ("APK_PKGREL", stamp["apk_pkgrel"]),
        ("ARCH_VERSION", stamp["arch_version"]),
        ("ARCH_PKGREL", stamp["arch_pkgrel"]),
    ]


def _self_test() -> None:
    """Pin the exact stamp strings every repository and retention rule relies on."""
    stable = stamps(base="0.2.0", channel="stable")
    nightly = stamps(
        base="0.2.0", channel="nightly", date="20261122", run=42, sha="90cb2db1234"
    )
    expected = {
        "version": "0.2.0~nightly20261122.r42+g90cb2db",
        "deb": "0.2.0~nightly20261122.r42+g90cb2db",
        "rpm_version": "0.2.0",
        "rpm_release": "0.nightly.20261122.r42.g90cb2db",
        "apk_version": "0.2.0_alpha2026112242",
        "apk_pkgrel": "0",
        "arch_version": "0.2.0r42.g90cb2db",
        "arch_pkgrel": "1",
    }
    assert nightly == expected, f"nightly stamps drifted: {nightly!r}"
    assert stable == {
        "version": "0.2.0",
        "deb": "0.2.0",
        "rpm_version": "0.2.0",
        "rpm_release": "1",
        "apk_version": "0.2.0",
        "apk_pkgrel": "0",
        "arch_version": "0.2.0",
        "arch_pkgrel": "1",
    }, f"stable stamps drifted: {stable!r}"
    # APT reads `~` as "sorts below the empty string", which is what keeps a
    # nightly below the release; the prefix before `~` must stay the base.
    assert nightly["deb"].split("~", 1) == ["0.2.0", "nightly20261122.r42+g90cb2db"]
    # Alpine rejects both `~` and `+` in pkgver.
    assert "~" not in nightly["apk_version"] and "+" not in nightly["apk_version"]
    # RPM's release starts below `1`, so `dnf upgrade` prefers the release.
    assert nightly["rpm_release"].startswith("0.nightly.")
    assert len(nightly["rpm_release"].split(".")) == 5
    print(
        "ok   release stamps: deb/rpm/apk/arch nightly sort below stable and match the contract"
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Compute Slipfork per-ecosystem version stamps."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    def add_common(p: argparse.ArgumentParser) -> None:
        p.add_argument("--base", required=True, help="release version, e.g. 0.2.0")
        p.add_argument("--channel", required=True, choices=CHANNELS)
        p.add_argument("--date", default="", help="UTC date as YYYYMMDD (nightly)")
        p.add_argument("--run", type=int, default=0, help="GITHUB_RUN_NUMBER (nightly)")
        p.add_argument(
            "--sha", default="", help="commit sha; the first 7 chars are stamped"
        )

    env = sub.add_parser("env", help="print shell assignments consumed by the Makefile")
    add_common(env)
    stamp = sub.add_parser("stamp", help="print one field")
    add_common(stamp)
    stamp.add_argument(
        "--field",
        required=True,
        choices=[
            "version",
            "deb",
            "rpm_version",
            "rpm_release",
            "apk_version",
            "apk_pkgrel",
            "arch_version",
            "arch_pkgrel",
        ],
    )
    sub.add_parser("self-test", help="assert the exact stamp strings")

    args = parser.parse_args(argv)
    try:
        if args.command == "self-test":
            _self_test()
            return 0
        result = stamps(
            base=args.base,
            channel=args.channel,
            date=args.date,
            run=args.run,
            sha=args.sha,
        )
    except StampError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    if args.command == "stamp":
        print(result[args.field])
        return 0
    for key, value in _env_names(result):
        print(f"{key}={_shell_quote(value)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
