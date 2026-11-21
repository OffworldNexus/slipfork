#!/usr/bin/env bash
# The portability gate for every ELF Slipfork ships. A package is a promise about
# *other* machines, so this script measures what the binary actually asks for:
#
#   * `DT_NEEDED` must be libc, libm, libgcc, the dynamic loader, or a pre-2.34
#     glibc companion object (libdl/libpthread/librt) that libc6 already owns.
#     `libpython*`/`libphp*` are refused: runtimes are dlopen'ed, never linked.
#   * `RPATH`/`RUNPATH` is refused: a staged binary needs no search path at all.
#     `--allow-origin-rpath` permits purely `$ORIGIN`-relative layouts.
#   * `--max-glibc <major>_<minor>` measures `.gnu.version_r` (`GLIBC_*`, weak
#     needs excluded) and refuses a file above the declared ceiling. The package
#     builders stamp `Depends:`/`Requires:` from the `max-glibc:` line, so no
#     floor is ever typed by hand.
#
# Stable output the builders parse:
#   max-glibc: 2.28 (/path/to/slipfork)
#   needed: libc.so.6 libgcc_s.so.1 libm.so.6 ld-linux-x86-64.so.2
#
# Usage: scripts/check-portable-elf.sh [--allow-origin-rpath]
#                                      [--max-glibc <major>_<minor>|auto] PATH...
set -euo pipefail

usage() {
    echo "usage: $0 [--allow-origin-rpath] [--max-glibc <major>_<minor>|auto] PATH..." >&2
}

allow_origin=0
glibc_ceiling=""
report_glibc=0
paths=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --allow-origin-rpath) allow_origin=1; shift ;;
        --max-glibc)
            [ "$#" -ge 2 ] && [ -n "$2" ] || { echo "error: --max-glibc needs <major>_<minor> or auto" >&2; exit 2; }
            report_glibc=1
            if [ "$2" != auto ]; then
                glibc_ceiling=${2//_/.}
                printf '%s' "$glibc_ceiling" | grep -Eq '^[0-9]+\.[0-9]+$' ||
                    { echo "error: --max-glibc '$2' is not <major>_<minor> (e.g. 2_28) or auto" >&2; exit 2; }
            fi
            shift 2 ;;
        -h | --help) usage; exit 0 ;;
        --*) echo "error: unknown option $1" >&2; usage; exit 2 ;;
        *) paths+=("$1"); shift ;;
    esac
done
[ "${#paths[@]}" -gt 0 ] || { usage; exit 2; }

status=0
measured_glibc=""
measured_from=""
measured_needed=""
for artifact in "${paths[@]}"; do
    artifact_status=0
    if [ ! -f "$artifact" ]; then
        echo "error: $artifact does not exist; next: build the bundle first" >&2
        status=1
        continue
    fi
    dynamic="$(readelf -d "$artifact")"

    for pattern in libpython libphp; do
        linked="$(printf '%s\n' "$dynamic" | grep -E "NEEDED.*$pattern" || true)"
        if [ -n "$linked" ]; then
            echo "error: $artifact links $pattern, so it only runs where that runtime is installed:" >&2
            printf '  %s\n' "$linked" >&2
            echo "  next: language runtimes are dlopen'ed or containerized; link nothing but libc/libm/libgcc" >&2
            artifact_status=1
        fi
    done

    runpath="$(printf '%s\n' "$dynamic" | grep -E '\((RUNPATH|RPATH)\)' || true)"
    if [ "$allow_origin" -eq 1 ] && [ -n "$runpath" ]; then
        filtered="$(printf '%s\n' "$runpath" | sed 's/\$ORIGIN[^ :]*//g')"
        if ! printf '%s' "$filtered" | grep -qE '\$|/'; then
            runpath=""
        fi
    fi
    if [ -n "$runpath" ]; then
        echo "error: $artifact carries a library search path from its build host:" >&2
        printf '  %s\n' "$runpath" >&2
        echo "  next: build without RPATH/RUNPATH; a staged binary links only system libraries" >&2
        artifact_status=1
    fi

    if [ "$report_glibc" -eq 1 ]; then
        artifact_floor="$(readelf --version-info "$artifact" 2>/dev/null \
            | sed -n 's/.*Name: GLIBC_\([0-9][0-9]*\(\.[0-9][0-9]*\)\+\)[[:space:]]\+Flags:[[:space:]]\+\([A-Za-z]*\).*/\1 \3/p' \
            | awk '$2 != "WEAK" { print $1 }' \
            | sort -V \
            | tail -n 1)"
        if [ -n "$artifact_floor" ]; then
            if [ -z "$measured_glibc" ] ||
                [ "$(printf '%s\n%s\n' "$artifact_floor" "$measured_glibc" | sort -V | tail -n 1)" = "$artifact_floor" ]; then
                measured_glibc="$artifact_floor"
                measured_from="$artifact"
            fi
        fi
    fi

    needed="$(printf '%s\n' "$dynamic" \
        | sed -n 's/.*(NEEDED) *Shared library: \[\(.*\)\]/\1/p' \
        | paste -sd ' ' -)"
    for library in $needed; do
        case "$library" in
            libc.so.6 | libm.so.6 | libgcc_s.so.1 | libdl.so.2 | libpthread.so.0 | librt.so.1 | \
                ld-linux-*.so* | ld64.so* | libc.musl-*.so*) ;;
            *)
                echo "error: $artifact needs $library, which no supported distribution guarantees" >&2
                echo "  next: link only libc, libm, libgcc, and the loader; everything else is dlopen'ed" >&2
                artifact_status=1 ;;
        esac
        measured_needed="$measured_needed $library"
    done

    if [ "$artifact_status" -eq 0 ]; then
        echo "ok   $artifact: NEEDED ${needed:-none}; no RPATH/RUNPATH"
    else
        status=1
    fi
done

if [ "$report_glibc" -eq 1 ]; then
    if [ -z "$measured_glibc" ]; then
        echo "max-glibc: none (no shipped ELF names a GLIBC_ symbol)"
    else
        echo "max-glibc: $measured_glibc ($measured_from)"
    fi
    echo "needed: $(printf '%s\n' $measured_needed | sort -u | paste -sd ' ' -)"
    if [ -n "$glibc_ceiling" ] && [ -n "$measured_glibc" ]; then
        if [ "$(printf '%s\n%s\n' "$measured_glibc" "$glibc_ceiling" | sort -V | tail -n 1)" != "$glibc_ceiling" ]; then
            echo "error: $measured_from needs glibc $measured_glibc, above the $glibc_ceiling ceiling" >&2
            echo "  next: build against an older sysroot, or raise --max-glibc and document the floor" >&2
            status=1
        fi
    fi
fi
exit "$status"
