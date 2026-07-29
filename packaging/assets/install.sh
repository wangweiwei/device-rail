#!/bin/sh
set -eu

prefix=${1:-"${HOME}/.local"}
case "${prefix}" in
  /*) ;;
  *)
    printf '%s\n' 'install prefix must be an absolute path' >&2
    exit 2
    ;;
esac

source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
install -d -m 0755 "${prefix}/bin"
install -m 0755 "${source_dir}/bin/devicerail-daemon" "${prefix}/bin/devicerail-daemon"
install -m 0755 "${source_dir}/bin/devicerail-bundle" "${prefix}/bin/devicerail-bundle"

printf 'Installed DeviceRail binaries in %s/bin\n' "${prefix}"
