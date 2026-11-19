#!/usr/bin/env bash
# Check every allocation budget: the value path's, the typed path's, and those of an untyped
# decode into a JSON tree.
#
# usage: tools/check_alloc_corpus.sh [MANIFEST [TYPED [JSON]]]
#
# MANIFEST is the value path's budgets, tools/alloc-corpus.tsv unless given. TYPED is the typed
# path's, tools/typed-alloc-corpus.tsv, and JSON those of an untyped decode into a JSON tree,
# tools/json-alloc-corpus.tsv. Each of the three files has to hold a budget. CI gives no
# argument; the gate's own tests give all three.
#
# The counts come from a counting allocator, so they don't depend on host load. A
# failure means a code change allocates more than its budget: either fix that, or
# raise the budget in the same change and say why.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
manifest="${1:-tools/alloc-corpus.tsv}"
typed_manifest="${2:-tools/typed-alloc-corpus.tsv}"
json_manifest="${3:-tools/json-alloc-corpus.tsv}"

checked=0

# A budget file that is missing, or that holds no row, would pass by checking nothing.
some_checked() {
    if [[ "$checked" -eq "$2" ]]; then
        echo "no budgets found in $1" >&2
        exit 1
    fi
}

# The second test keeps a last row that has no trailing newline.
check_file() {
    local file="$1" typed="$2" before="$checked"
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
    some_checked "$file" "$before"
}

# The untyped decode into a JSON tree has three budgets a row: its peak, its allocations, and
# the bytes it allocates in total.
check_json_file() {
    local file="$1" before="$checked"
    while IFS=$'\t' read -r path json_peak json_allocs json_total || [[ -n "$path" ]]; do
        case "$path" in
            "" | \#*) continue ;;
        esac
        env AYAML_JSON=1 cargo run --quiet --release --locked --manifest-path fuzz/Cargo.toml --bin memcount -- \
            "$path" "$json_peak" "$json_allocs" "$json_total"
        checked=$((checked + 1))
    done < "$file"
    some_checked "$file" "$before"
}

check_file "$manifest" value
check_file "$typed_manifest" typed
check_json_file "$json_manifest"
echo "checked $checked allocation budgets"
