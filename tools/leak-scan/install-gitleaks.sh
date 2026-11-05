#!/usr/bin/env bash
# Install the pinned gitleaks after verifying its archive digest, then print the
# path to the binary. Usage: install-gitleaks.sh INSTALL_DIR
set -euo pipefail

script_dir="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "$script_dir/gitleaks.env"

if [[ "$#" -ne 1 ]]; then
    echo "usage: $0 INSTALL_DIR" >&2
    exit 2
fi
install_dir="$1"
binary="$install_dir/gitleaks"

if [[ "$(uname -s):$(uname -m)" != "Linux:x86_64" ]]; then
    echo "error: this script pins only the Linux x86_64 build; install gitleaks $GITLEAKS_VERSION yourself" >&2
    exit 1
fi

if [[ -x "$binary" && "$("$binary" version 2>/dev/null)" == "$GITLEAKS_VERSION" ]]; then
    echo "$binary"
    exit 0
fi

mkdir -p "$install_dir"
work="$(mktemp -d "$install_dir/.download.XXXXXX")"
trap 'rm -rf "$work"' EXIT

archive="gitleaks_${GITLEAKS_VERSION}_linux_x64.tar.gz"
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error --retry 3 \
    --output "$work/$archive" \
    "https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/${archive}"
printf '%s  %s\n' "$GITLEAKS_LINUX_X64_SHA256" "$work/$archive" | sha256sum --check --quiet -
tar -xzf "$work/$archive" -C "$work" gitleaks
install -m 0755 "$work/gitleaks" "$binary"

if [[ "$("$binary" version)" != "$GITLEAKS_VERSION" ]]; then
    echo "error: the installed binary does not report version $GITLEAKS_VERSION" >&2
    exit 1
fi
echo "$binary"
