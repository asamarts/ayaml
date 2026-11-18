#!/usr/bin/env bash
# Check every allocation budget in tools/alloc-corpus.tsv.
#
# usage: tools/check_alloc_corpus.sh [MANIFEST]
#
# The counts come from a counting allocator, so they don't depend on host load. A
# failure means a code change allocates more than its budget: either fix that, or
# raise the budget in the same change and say why.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
manifest="${1:-tools/alloc-corpus.tsv}"

checked=0
# The second test keeps a last row that has no trailing newline.
check_file() {
    local file="$1" typed="$2"
    while IFS=$'\t' read -r path parse_peak emit_peak parse_allocs emit_allocs serialize_peak serialize_allocs parse_total emit_total serialize_total || [[ -n "$path" ]]; do
        case "$path" in
            "" | \#*) continue ;;
        esac
        local env=()
        if [[ "$typed" == "typed" ]]; then
            env=(env AYAML_TYPED=1)
        fi
        "${env[@]}" cargo run --quiet --release --locked --manifest-path fuzz/Cargo.toml --bin memcount -- \
            "$path" \
            "$parse_peak" "$emit_peak" "$parse_allocs" "$emit_allocs" \
            "$serialize_peak" "$serialize_allocs" \
            "$parse_total" "$emit_total" "$serialize_total"
        checked=$((checked + 1))
    done < "$file"
}

check_file "$manifest" value
if [[ -f tools/typed-alloc-corpus.tsv ]]; then
    check_file tools/typed-alloc-corpus.tsv typed
fi

if [[ "$checked" -eq 0 ]]; then
    echo "no budgets found in $manifest" >&2
    exit 1
fi
echo "checked $checked allocation budgets"
