#!/usr/bin/env bash
# Install the pre-push hook for this clone and all its worktrees.
#
# The hook lives in the clone's Git directory and runs the gate as it is on
# origin/main, so a branch that predates the gate, or that edits it, can't turn it
# off. It fails closed when origin/main or the gate can't be read.
set -euo pipefail

common="$(git rev-parse --path-format=absolute --git-common-dir)"

# core.hooksPath would override the installed hook.
git config --unset core.hooksPath || true

mkdir -p "$common/hooks"
cat >"$common/hooks/pre-push" <<'HOOK'
#!/usr/bin/env bash
# Installed by tools/leak-scan/install-hook.sh. Runs the leak gate from origin/main.
set -euo pipefail

trusted="refs/remotes/origin/main"
if ! git rev-parse --verify --quiet "$trusted^{commit}" >/dev/null; then
    echo "pre-push: $trusted is missing, so the leak gate did not run; run git fetch origin" >&2
    exit 2
fi

common="$(git rev-parse --path-format=absolute --git-common-dir)"
work="$(mktemp -d "$common/leak-gate.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT

if ! git archive "$trusted" tools/hooks tools/leak-scan .gitleaks.toml </dev/null | tar -x -C "$work" -f -; then
    echo "pre-push: cannot read the leak gate from $trusted, so it did not run" >&2
    exit 2
fi

"$work/tools/hooks/pre-push" "$@"
HOOK
chmod +x "$common/hooks/pre-push"
echo "installed the leak gate hook in $common/hooks/pre-push"
