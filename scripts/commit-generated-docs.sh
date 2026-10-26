#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
message="${1:?Commit message is required}"
shift
if (( $# == 0 )); then
    echo 'Generated file paths are required.' >&2
    exit 1
fi
branch="${GITHUB_REF_NAME:?Branch is required}"

# Container jobs can run as a different owner from actions/checkout.
# Scope trust and bot identity to this script, including rebase on push retries.
git() {
    command git -c safe.directory="$root" \
        -c user.name='github-actions[bot]' \
        -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
        -c commit.gpgsign=false "$@"
}
git add -- "$@"
if git diff --cached --quiet -- "$@"; then
    echo 'Generated documentation is up to date.'
    exit 0
fi
git commit --only -m "$message" -- "$@"

# Another push may land while documentation is being generated.
for attempt in 1 2 3; do
    if git push origin "HEAD:$branch"; then exit 0; fi
    if (( attempt == 3 )); then exit 1; fi
    git fetch origin "$branch"
    git rebase FETCH_HEAD
done
