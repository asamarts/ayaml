#!/usr/bin/env bash
# Run one fuzz target on hosted CI without publishing what it finds.
#
# usage: tools/fuzz-hosted.sh TARGET SECONDS OUTPUT_DIRECTORY
#
# Whatever the fuzzer prints can hold the input, and a crash file's name is a hash that a short
# input could be guessed from. So the public log gets only the target, the exit status, the run
# count, and the hash of the encrypted file. On a failure, the fuzzer's log and its crash files go
# to OUTPUT_DIRECTORY, encrypted to tools/fuzz-crash-recipient.txt. If that fails, nothing is kept,
# because a lost input is better than a published one. See D74.
set -uo pipefail

if [[ $# -ne 3 || ! "$1" =~ ^[a-z_]+$ || ! "$2" =~ ^[0-9]+$ ]]; then
    echo "usage: tools/fuzz-hosted.sh TARGET SECONDS OUTPUT_DIRECTORY" >&2
    exit 2
fi
target="$1"
seconds="$2"
output="$3"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
artifacts="$root/fuzz/artifacts"
work="$(mktemp -d)" || exit 2
trap 'rm -rf "$work"' EXIT

cd "$root" || exit 2
cargo fuzz run "$target" --target x86_64-unknown-linux-gnu -- \
    -max_total_time="$seconds" -print_final_stats=1 >"$work/fuzz.log" 2>&1
status=$?

runs="$(sed -n 's/^stat::number_of_executed_units: *\([0-9]*\)$/\1/p' "$work/fuzz.log" | tail -n1)"
printf 'target=%s exit=%s runs=%s\n' "$target" "$status" "${runs:-unknown}"
if [[ "$status" -eq 0 ]]; then
    exit 0
fi

sealed="$output/fuzz-$target.tar.gz.age"
packed=()
[[ -d "$artifacts/$target" ]] && packed=(-C "$artifacts" "$target")
if mkdir -p "$output" &&
    tar -czf "$work/found.tar.gz" -C "$work" fuzz.log "${packed[@]}" &&
    age -R "$root/tools/fuzz-crash-recipient.txt" -o "$sealed" "$work/found.tar.gz"; then
    printf 'encrypted=%s sha256=%s\n' "${sealed##*/}" "$(sha256sum "$sealed" | cut -d' ' -f1)"
else
    rm -f "$sealed"
    echo "could not encrypt what the fuzzer found, so nothing was kept" >&2
fi
# Leave no plain copy for a later step to pick up.
rm -rf "${artifacts:?}/$target"
exit "$status"
