#!/usr/bin/env bash
# The GitHub release half of release.yml. A release and its rehearsal run these same commands, so a
# rehearsal shows that the release job's steps still work. Run it from the repository's root, with
# GH_TOKEN and GH_REPO set for gh.
#
#   tools/github_release.sh fetch VERSION DIGEST DIST
#       Downloads the crate crates.io serves into DIST, checks it as `check_release.py registry`
#       does and against DIGEST, and writes DIST/SHA256SUMS.
#   tools/github_release.sh release TAG VERSION DIST NOTES
#       Creates the release of TAG with the crate and SHA256SUMS of DIST and the notes in NOTES. A
#       release that exists, as a rerun finds it, must hold exactly those assets with these bytes,
#       and if it's a draft it's then published.
#   tools/github_release.sh rehearse NAME TARGET VERSION DIST NOTES
#       Creates a draft named NAME the same way, checks it as a rerun checks a release, and deletes
#       it. NAME is rehearsal-RUN-ATTEMPT, which no release tag can be. A draft makes no tag, but
#       GitHub refused a job's token one whose tag would be made at a commit with other workflow
#       files than the default branch's, so TARGET is the default branch.
#   tools/github_release.sh discard NAME
#       Deletes the draft NAME, if a rehearsal stopped before it could, and nothing else.
set -euo pipefail

tools="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
rehearsal_name='^rehearsal-[0-9]+-[0-9]+$'

fail() {
  echo "::error::$*" >&2
  exit 1
}

fetch() {
  local version="$1" digest="$2" dist="$3"
  local crate="$dist/ayaml-$version.crate"
  mkdir -p "$dist"
  curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error --retry 3 \
    --user-agent "ayaml-release (https://github.com/asamarts/ayaml)" \
    --output "$crate" "https://static.crates.io/crates/ayaml/ayaml-$version.crate"
  python3 "$tools/check_release.py" registry "$crate" "$version" 600
  [ "$(sha256sum "$crate" | cut -d' ' -f1)" = "$digest" ] ||
    fail "crates.io serves other bytes for ayaml $version than the package job built; see RELEASING.md"
  (cd "$dist" && sha256sum "ayaml-$version.crate" > SHA256SUMS)
}

# Whether gh finds a release or a draft named NAME.
exists() {
  gh release view "$1" --json isDraft > /dev/null 2>&1
}

is_draft() {
  [ "$(gh release view "$1" --json isDraft --jq .isDraft)" = true ]
}

# NAME holds exactly the crate and SHA256SUMS of DIST, with these bytes.
check_assets() {
  local name="$1" version="$2" dist="$3" assets existing
  assets="$(gh release view "$name" --json assets --jq '[.assets[].name] | sort | join(" ")')"
  [ "$assets" = "SHA256SUMS ayaml-$version.crate" ] ||
    fail "release $name holds '$assets', not this crate and SHA256SUMS; see RELEASING.md"
  existing="$(mktemp -d)"
  gh release download "$name" --dir "$existing" --pattern "ayaml-$version.crate" --pattern SHA256SUMS
  cmp "$existing/ayaml-$version.crate" "$dist/ayaml-$version.crate"
  cmp "$existing/SHA256SUMS" "$dist/SHA256SUMS"
  rm -r "$existing"
}

release() {
  local tag="$1" version="$2" dist="$3" notes="$4"
  local digest
  digest="$(cut -d' ' -f1 "$dist/SHA256SUMS")"
  # gh finds a draft too. gh creates a release as a draft, uploads its assets, and then publishes
  # it, so a run stopped in between leaves a draft.
  if exists "$tag"; then
    check_assets "$tag" "$version" "$dist"
    if is_draft "$tag"; then
      gh release edit "$tag" --draft=false
      echo "::notice::published the draft release $tag, which holds ayaml-$version.crate with sha256 $digest"
    else
      echo "::notice::release $tag already holds ayaml-$version.crate with sha256 $digest"
    fi
    return
  fi
  gh release create "$tag" --verify-tag --title "ayaml $version" --notes-file "$notes" \
    "$dist/ayaml-$version.crate" "$dist/SHA256SUMS"
}

# The draft a rehearsal made, for the trap to delete if a step fails.
draft=""

discard_draft() {
  if [ -n "$draft" ] && exists "$draft" && is_draft "$draft"; then
    gh release delete "$draft" --yes
  fi
}

rehearse() {
  local name="$1" target="$2" version="$3" dist="$4" notes="$5"
  [[ "$name" =~ $rehearsal_name ]] || fail "$name isn't a rehearsal's name, rehearsal-RUN-ATTEMPT"
  ! exists "$name" || fail "a release named $name exists already"
  draft="$name"
  trap discard_draft EXIT
  gh release create "$name" --draft --target "$target" --title "ayaml $version (rehearsal)" \
    --notes-file "$notes" "$dist/ayaml-$version.crate" "$dist/SHA256SUMS"
  is_draft "$name" || fail "$name was created, but not as a draft"
  check_assets "$name" "$version" "$dist"
  # Both sides without their trailing line breaks, which GitHub may not keep.
  [ "$(gh release view "$name" --json body --jq .body)" = "$(cat "$notes")" ] ||
    fail "the draft's notes aren't the notes it was given"
  gh release delete "$name" --yes
  draft=""
  ! exists "$name" || fail "the draft $name is still there after its deletion"
  echo "::notice::a draft release of ayaml $version held the crate and SHA256SUMS that crates.io serves, and is deleted"
}

discard() {
  [[ "$1" =~ $rehearsal_name ]] || fail "$1 isn't a rehearsal's name, rehearsal-RUN-ATTEMPT"
  draft="$1"
  discard_draft
}

usage() {
  sed -n '2,20s/^# \{0,1\}//p' "$0" >&2
  exit 2
}

# Each command runs on its own, never inside `||` or `&&`, where bash would ignore `set -e` in it.
command="${1:-}"
[ $# -eq 0 ] || shift
case "$command" in
  fetch) [ $# -eq 3 ] || usage ;;
  release) [ $# -eq 4 ] || usage ;;
  rehearse) [ $# -eq 5 ] || usage ;;
  discard) [ $# -eq 1 ] || usage ;;
  *) usage ;;
esac
"$command" "$@"
