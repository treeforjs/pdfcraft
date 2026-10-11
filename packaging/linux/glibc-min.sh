#!/usr/bin/env bash
# Print the Debian glibc dependency floor of the binaries, with the release baseline as minimum.
# Usage: packaging/linux/glibc-min.sh BINARY [BINARY ...]
set -euo pipefail
if (( $# == 0 )); then
  echo "usage: $0 BINARY [BINARY ...]" >&2
  exit 2
fi
versions="$(LC_ALL=C readelf --version-info -- "$@" | sed -n 's/.*Name: GLIBC_\([0-9][0-9.]*\) .*/\1/p')"
printf '%s\n' 2.35 "$versions" | sort -Vu | tail -n 1
