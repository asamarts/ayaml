#!/usr/bin/env bash
# The GitHub release half of release.yml. A release and its rehearsal run these same commands, so a
# rehearsal shows that the release job's steps still work. Run it from the repository's root, with
# GH_TOKEN and GH_REPO set for gh.
#
#   tools/github_release.sh fetch VERSION DIGEST DIST [PKG=DIGEST ...]
#       Downloads the crate(s) crates.io serves into DIST (always ayaml, plus any extra PKG=DIGEST
#       pairs such as ayaml-edit=<sha256>), checks each as `check_release.py registry` does and
#       against its digest, and writes DIST/SHA256SUMS.
#   tools/github_release.sh release TAG VERSION DIST NOTES
#       Creates the release of TAG with the crate(s) and SHA256SUMS of DIST and the notes in NOTES,
#       and checks what it made. A release that exists, as a rerun finds it, must hold exactly those
#       assets with these bytes, and if it's a draft it's then published.
#   tools/github_release.sh rehearse NAME TARGET VERSION DIST NOTES
#       Creates a draft named NAME the same way, checks it as a rerun checks a release, and deletes
#       it. NAME is rehearsal-RUN-ATTEMPT, which no release tag can be. TARGET is the default
#       branch: GitHub refuses a job's token a release, draft or not, whose target commit adds or
#       changes workflow files relative to the default branch.
#   tools/github_release.sh discard NAME
#       Deletes the draft NAME, if a rehearsal stopped before it could, and nothing else. It warns
#       of any other rehearsal's draft that is still there.
#
# Each lookup tells a release that isn't there from an error of gh's, and stops on the error.
set -euo pipefail

tools="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
rehearsal_name='^rehearsal-[0-9]+-[0-9]+$'

fail() {
  echo "::error::$*" >&2
  exit 1
}

fetch_one() {
  local pkg="$1" version="$2" digest="$3" dist="$4"
  local crate="$dist/$pkg-$version.crate"
  curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error --retry 3 \
    --user-agent "ayaml-release (https://github.com/asamarts/ayaml)" \
    --output "$crate" "https://static.crates.io/crates/$pkg/$pkg-$version.crate"
  if [ "$pkg" = "ayaml" ]; then
    python3 "$tools/check_release.py" registry "$crate" "$version" 600
  else
    python3 "$tools/check_release.py" registry --package "$pkg" "$crate" "$version" 600
  fi
  [ "$(sha256sum "$crate" | cut -d' ' -f1)" = "$digest" ] ||
    fail "crates.io serves other bytes for $pkg $version than the package job built; see RELEASING.md"
}

fetch() {
  local version="$1" digest="$2" dist="$3"
  shift 3
  mkdir -p "$dist"
  local crates=("ayaml-$version.crate")
  fetch_one "ayaml" "$version" "$digest" "$dist"
  local spec pkg extra_digest
  for spec in "$@"; do
    [[ "$spec" == *=* ]] || fail "invalid extra package spec '$spec'; expected PKG=DIGEST"
    pkg="${spec%%=*}"
    extra_digest="${spec#*=}"
    [ -n "$pkg" ] && [ -n "$extra_digest" ] || fail "invalid extra package spec '$spec'"
    fetch_one "$pkg" "$version" "$extra_digest" "$dist"
    crates+=("$pkg-$version.crate")
  done
  local sorted_crates=()
  mapfile -t sorted_crates < <(printf '%s\n' "${crates[@]}" | LC_ALL=C sort)
  (cd "$dist" && sha256sum "${sorted_crates[@]}" > SHA256SUMS)
}

# Sets `found` to draft, published, or absent for NAME, which gh finds by its tag's name, draft or
# not. Anything else gh says stops the script, since a release gh couldn't see may still be there.
found=""
look_up() {
  local name="$1" answer errors
  errors="$(mktemp)"
  found=""
  if answer="$(gh release view "$name" --json isDraft --jq .isDraft 2> "$errors")"; then
    case "$answer" in
      true) found=draft ;;
      false) found=published ;;
    esac
  elif grep -qx 'release not found' "$errors"; then
    found=absent
  fi
  if [ -z "$found" ]; then
    cat "$errors" >&2
    rm -f "$errors"
    fail "gh couldn't say whether release $name is there; rerun the job"
  fi
  rm -f "$errors"
}

# Sorted basenames of the release assets in DIST (`SHA256SUMS` plus every `*-<version>.crate`).
dist_asset_names() {
  local version="$1" dist="$2"
  local names=() path base
  shopt -s nullglob
  for path in "$dist"/*-"$version".crate; do
    base="$(basename "$path")"
    names+=("$base")
  done
  shopt -u nullglob
  [ "${#names[@]}" -gt 0 ] || fail "no *-$version.crate files found in $dist"
  [ -f "$dist/SHA256SUMS" ] || fail "SHA256SUMS missing from $dist"
  names+=("SHA256SUMS")
  printf '%s\n' "${names[@]}" | LC_ALL=C sort
}

# NAME holds exactly the crate(s) and SHA256SUMS of DIST, with these bytes.
check_assets() {
  local name="$1" version="$2" dist="$3" assets existing expected_list
  local expected=() download_args=() asset
  mapfile -t expected < <(dist_asset_names "$version" "$dist")
  expected_list="$(printf '%s ' "${expected[@]}")"
  expected_list="${expected_list% }"
  assets="$(gh release view "$name" --json assets --jq '[.assets[].name] | sort | join(" ")')"
  [ "$assets" = "$expected_list" ] ||
    fail "release $name holds '$assets', not '$expected_list'; see RELEASING.md"
  existing="$(mktemp -d)"
  for asset in "${expected[@]}"; do
    download_args+=(--pattern "$asset")
  done
  gh release download "$name" --dir "$existing" "${download_args[@]}"
  for asset in "${expected[@]}"; do
    cmp "$existing/$asset" "$dist/$asset"
  done
  rm -r "$existing"
}

dist_upload_paths() {
  local version="$1" dist="$2"
  local path
  shopt -s nullglob
  for path in "$dist"/*-"$version".crate; do
    printf '%s\n' "$path"
  done | LC_ALL=C sort
  shopt -u nullglob
  printf '%s\n' "$dist/SHA256SUMS"
}

release() {
  local tag="$1" version="$2" dist="$3" notes="$4"
  local digest upload_paths=()
  digest="$(head -n1 "$dist/SHA256SUMS" | cut -d' ' -f1)"
  mapfile -t upload_paths < <(dist_upload_paths "$version" "$dist")
  look_up "$tag"
  case "$found" in
    absent)
      gh release create "$tag" --verify-tag --title "ayaml $version" --notes-file "$notes" \
        "${upload_paths[@]}"
      look_up "$tag"
      [ "$found" = published ] || fail "gh created the release $tag, but it is $found"
      check_assets "$tag" "$version" "$dist"
      echo "::notice::created the release $tag, which holds ayaml-$version.crate with sha256 $digest"
      ;;
    draft)
      # gh creates a release as a draft, uploads its assets, and then publishes it, so a run
      # stopped in between leaves a draft.
      check_assets "$tag" "$version" "$dist"
      gh release edit "$tag" --draft=false
      look_up "$tag"
      [ "$found" = published ] || fail "the draft release $tag is still $found after it was published"
      echo "::notice::published the draft release $tag, which holds ayaml-$version.crate with sha256 $digest"
      ;;
    published)
      check_assets "$tag" "$version" "$dist"
      echo "::notice::release $tag already holds ayaml-$version.crate with sha256 $digest"
      ;;
  esac
}

# The draft a rehearsal made, for the trap to delete if a step fails.
draft=""

# Deletes the draft `draft` if it's there. A release with a rehearsal's name that isn't a draft is
# left for a writer to look at.
discard_draft() {
  [ -n "$draft" ] || return 0
  look_up "$draft"
  case "$found" in
    draft)
      gh release delete "$draft" --yes
      look_up "$draft"
      [ "$found" = absent ] || fail "the draft $draft is still there after its deletion"
      echo "::notice::deleted the draft release $draft"
      ;;
    absent)
      echo "::notice::there is no draft release $draft to delete"
      ;;
    published)
      echo "::warning::release $draft is published, which a rehearsal never does, so it's left as it is; see RELEASING.md"
      ;;
  esac
  draft=""
}

rehearse() {
  local name="$1" target="$2" version="$3" dist="$4" notes="$5"
  local body upload_paths=()
  [[ "$name" =~ $rehearsal_name ]] || fail "$name isn't a rehearsal's name, rehearsal-RUN-ATTEMPT"
  mapfile -t upload_paths < <(dist_upload_paths "$version" "$dist")
  look_up "$name"
  [ "$found" = absent ] || fail "a release named $name exists already"
  draft="$name"
  trap discard_draft EXIT
  gh release create "$name" --draft --target "$target" --title "ayaml $version (rehearsal)" \
    --notes-file "$notes" "${upload_paths[@]}"
  look_up "$name"
  [ "$found" = draft ] || fail "$name was created, but it is $found, not a draft"
  check_assets "$name" "$version" "$dist"
  body="$(gh release view "$name" --json body --jq .body)"
  # Both sides without their trailing line breaks, which GitHub may not keep.
  [ "$body" = "$(cat "$notes")" ] || fail "the draft's notes aren't the notes it was given"
  discard_draft
  echo "::notice::a draft release of ayaml $version held the crate and SHA256SUMS that crates.io serves, and is deleted"
}

discard() {
  local name="$1" drafts other
  [[ "$name" =~ $rehearsal_name ]] || fail "$name isn't a rehearsal's name, rehearsal-RUN-ATTEMPT"
  draft="$name"
  discard_draft
  # A rehearsal that stopped before this step, as one does when its runner is lost, leaves its draft.
  drafts="$(gh release list --limit 100 --json tagName,isDraft --jq '.[] | select(.isDraft) | .tagName')"
  for other in $drafts; do
    if [[ "$other" =~ $rehearsal_name ]]; then
      echo "::warning::the draft release $other of another rehearsal is still there; delete it under Releases once no rehearsal is running"
    fi
  done
}

usage() {
  sed -n '2,/^set /{/^#/s/^# \{0,1\}//p;}' "$0" >&2
  exit 2
}

# Each command runs on its own, never inside `||` or `&&`, where bash would ignore `set -e` in it.
command="${1:-}"
[ $# -eq 0 ] || shift
case "$command" in
  fetch) [ $# -ge 3 ] || usage ;;
  release) [ $# -eq 4 ] || usage ;;
  rehearse) [ $# -eq 5 ] || usage ;;
  discard) [ $# -eq 1 ] || usage ;;
  *) usage ;;
esac
"$command" "$@"
