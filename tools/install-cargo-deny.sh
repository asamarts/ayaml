#!/usr/bin/env bash
# Install the pinned cargo-deny after verifying its archive digest, then print the directory that
# holds it, for PATH. Usage: install-cargo-deny.sh INSTALL_DIR
#
# CI installs it this way rather than through its GitHub Action, whose image downloads the binary
# with no checksum from a release that can still change. Nothing updates this pin: move VERSION and
# the digest together, taking the digest from the release asset and checking it against GitHub's.
set -euo pipefail

VERSION="0.20.2"
LINUX_X64_SHA256="9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f"

if [[ "$#" -ne 1 ]]; then
    echo "usage: $0 INSTALL_DIR" >&2
    exit 2
fi
install_dir="$1"
binary="$install_dir/cargo-deny"

if [[ "$(uname -s):$(uname -m)" != "Linux:x86_64" ]]; then
    echo "error: this script pins only the Linux x86_64 build; install cargo-deny $VERSION yourself" >&2
    exit 1
fi

name="cargo-deny-${VERSION}-x86_64-unknown-linux-musl"
archive="$install_dir/$name.tar.gz"

# An earlier run's binary is used only while the archive kept beside it has the pinned digest and
# holds that very binary.
if [[ -x "$binary" && -f "$archive" ]] &&
    printf '%s  %s\n' "$LINUX_X64_SHA256" "$archive" | sha256sum --check --status - &&
    tar -xzOf "$archive" "$name/cargo-deny" | cmp -s - "$binary"; then
    echo "$install_dir"
    exit 0
fi

mkdir -p "$install_dir"
work="$(mktemp -d "$install_dir/.download.XXXXXX")"
trap 'rm -rf "$work"' EXIT

curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error --retry 3 \
    --output "$work/$name.tar.gz" \
    "https://github.com/EmbarkStudios/cargo-deny/releases/download/${VERSION}/${name}.tar.gz"
printf '%s  %s\n' "$LINUX_X64_SHA256" "$work/$name.tar.gz" | sha256sum --check --quiet -
tar -xzf "$work/$name.tar.gz" -C "$work" "$name/cargo-deny"
install -m 0755 "$work/$name/cargo-deny" "$binary"
mv "$work/$name.tar.gz" "$archive"

if [[ "$("$binary" --version)" != "cargo-deny $VERSION" ]]; then
    echo "error: the installed binary does not report version $VERSION" >&2
    exit 1
fi
echo "$install_dir"
