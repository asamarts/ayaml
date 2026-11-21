#!/usr/bin/env bash
# Build and install the pinned public-api listing tool after verifying the crates.io archive
# digest, then print the directory that holds it, for PATH.
# Usage: install-public-api.sh INSTALL_DIR
#
# Uses the pure-Rust `public-api` library crate from the cargo-public-api project directly,
# locked to the Cargo.lock inside the verified `.crate` archive, so the tool needs no system
# OpenSSL or libcurl headers and builds identically across Linux and macOS hosts.
# Nothing updates this pin: move VERSION, CRATE_SHA256, and TOOLCHAIN together, matching the
# rustdoc JSON format version that `public-api` reads.
set -euo pipefail

VERSION="0.52.0"
CRATE_SHA256="02f66902549074a24229cb6d3e5b69f823fa620ec527b6a10708c1798d31e273"
TOOLCHAIN="nightly-2026-05-21"

if [[ "$#" -ne 1 ]]; then
    echo "usage: $0 INSTALL_DIR" >&2
    exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cli_source="$script_dir/public-api-cli.rs"
if [[ ! -f "$cli_source" ]]; then
    echo "error: missing $cli_source" >&2
    exit 1
fi

install_dir="$1"
binary="$install_dir/ayaml-public-api"
archive="public-api-${VERSION}.crate"
kept="$install_dir/$archive"
stamp="$install_dir/ayaml-public-api.stamp"

cli_sha256="$(sha256sum "$cli_source" | awk '{print $1}')"

# Reuse an earlier build only while the kept crate archive, CLI source, toolchain, and binary
# all match their recorded digests.
if [[ -x "$binary" && -f "$kept" && -f "$stamp" ]] &&
    printf '%s  %s\n' "$CRATE_SHA256" "$kept" | sha256sum --check --status -; then
    bin_sha256="$(sha256sum "$binary" | awk '{print $1}')"
    expected_stamp="$(printf 'version=%s\ntoolchain=%s\ncrate=%s\ncli=%s\nbin=%s\n' \
        "$VERSION" "$TOOLCHAIN" "$CRATE_SHA256" "$cli_sha256" "$bin_sha256")"
    if [[ "$(cat "$stamp")" == "$expected_stamp" ]]; then
        echo "$install_dir"
        exit 0
    fi
fi

if ! cargo "+$TOOLCHAIN" --version >/dev/null 2>&1; then
    echo "error: toolchain $TOOLCHAIN is required; install it with: rustup toolchain install $TOOLCHAIN" >&2
    exit 1
fi

mkdir -p "$install_dir"
work="$(mktemp -d "$install_dir/.build.XXXXXX")"
trap 'rm -rf "$work"' EXIT

if [[ -f "$kept" ]] && printf '%s  %s\n' "$CRATE_SHA256" "$kept" | sha256sum --check --status -; then
    cp "$kept" "$work/$archive"
else
    curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error --retry 3 \
        --output "$work/$archive" \
        "https://static.crates.io/crates/public-api/${archive}"
    printf '%s  %s\n' "$CRATE_SHA256" "$work/$archive" | sha256sum --check --quiet -
fi

tar -xzf "$work/$archive" -C "$work"
crate_dir="$work/public-api-${VERSION}"
cp "$cli_source" "$crate_dir/src/bin_cli.rs"
cat >>"$crate_dir/Cargo.toml" <<'EOF'

[[bin]]
name = "ayaml-public-api"
path = "src/bin_cli.rs"

[workspace]
EOF

RUSTFLAGS="" CARGO_TARGET_DIR="$work/target" cargo "+$TOOLCHAIN" build \
    --quiet --release --locked --no-default-features \
    --bin ayaml-public-api \
    --manifest-path "$crate_dir/Cargo.toml"

install -m 0755 "$work/target/release/ayaml-public-api" "$binary"
mv -f "$work/$archive" "$kept"

if [[ "$("$binary" --version)" != "ayaml-public-api $VERSION" ]]; then
    echo "error: the installed binary does not report version $VERSION" >&2
    exit 1
fi

bin_sha256="$(sha256sum "$binary" | awk '{print $1}')"
printf 'version=%s\ntoolchain=%s\ncrate=%s\ncli=%s\nbin=%s\n' \
    "$VERSION" "$TOOLCHAIN" "$CRATE_SHA256" "$cli_sha256" "$bin_sha256" >"$stamp"

echo "$install_dir"
