#!/usr/bin/env bash
# Install the pinned cargo-semver-checks after verifying its archive digest, then print the
# directory that holds it, for PATH. Usage: install-semver-checks.sh INSTALL_DIR
#
# CI installs it this way rather than through its GitHub Action, which installs the latest
# release unless one is already on PATH.
set -euo pipefail

VERSION="0.51.0"
LINUX_X64_SHA256="cacadafaa5cd27ae6d05a74a103266917ebab4c5e43c898360e177d4fbf06a65"

if [[ "$#" -ne 1 ]]; then
    echo "usage: $0 INSTALL_DIR" >&2
    exit 2
fi
install_dir="$1"
binary="$install_dir/cargo-semver-checks"

if [[ "$(uname -s):$(uname -m)" != "Linux:x86_64" ]]; then
    echo "error: this script pins only the Linux x86_64 build; install cargo-semver-checks $VERSION yourself" >&2
    exit 1
fi

archive="cargo-semver-checks-x86_64-unknown-linux-gnu.tar.gz"
kept="$install_dir/$archive"

# An earlier run's binary is used only while the archive kept beside it has the pinned digest and
# holds that very binary.
if [[ -x "$binary" && -f "$kept" ]] &&
    printf '%s  %s\n' "$LINUX_X64_SHA256" "$kept" | sha256sum --check --status - &&
    tar -xzOf "$kept" cargo-semver-checks | cmp -s - "$binary"; then
    echo "$install_dir"
    exit 0
fi

mkdir -p "$install_dir"
work="$(mktemp -d "$install_dir/.download.XXXXXX")"
trap 'rm -rf "$work"' EXIT

curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error --retry 3 \
    --output "$work/$archive" \
    "https://github.com/obi1kenobi/cargo-semver-checks/releases/download/v${VERSION}/${archive}"
printf '%s  %s\n' "$LINUX_X64_SHA256" "$work/$archive" | sha256sum --check --quiet -
tar -xzf "$work/$archive" -C "$work" cargo-semver-checks
install -m 0755 "$work/cargo-semver-checks" "$binary"
mv "$work/$archive" "$kept"

if [[ "$("$binary" --version)" != "cargo-semver-checks $VERSION" ]]; then
    echo "error: the installed binary does not report version $VERSION" >&2
    exit 1
fi
echo "$install_dir"
