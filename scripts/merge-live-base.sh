#!/usr/bin/env bash
# Bring a pull-request checkout up to the live tip of its base branch, so the
# mounted Domain checks (.github/workflows/domains.yml) test a consistent pair.
#
# A pull request checks out GitHub's merge ref, which GitHub computed when the
# PR was last pushed. Each Domain is checked out at its live main, so a Platform
# change that merged with its Domain companion afterwards leaves the merge ref
# older than the Domain, and the pair fails for reasons the PR did not cause.
# Merging the live base into the checkout makes the Platform side as new as the
# Domain side. Usage: merge-live-base.sh <base-branch> [remote]
set -euo pipefail
base=${1:?usage: merge-live-base.sh <base-branch> [remote]}
remote=${2:-origin}

git fetch --quiet --no-tags "$remote" "+refs/heads/${base}:refs/remotes/${remote}/${base}"
tip=$(git rev-parse "refs/remotes/${remote}/${base}")

if git merge-base --is-ancestor "$tip" HEAD; then
  echo "This checkout already contains the live ${base} (${tip:0:12})."
  exit 0
fi

echo "Merging the live ${base} (${tip:0:12}) into this checkout (${base} moved after the merge ref was made)."
if ! git -c user.name=github-actions -c user.email=github-actions@github.com \
  merge --quiet --no-edit "$tip"; then
  git merge --abort 2>/dev/null || true
  message="This pull request conflicts with the live ${base}, which moved after the pull request was last updated. Update the branch (merge or rebase ${base}), resolve the conflicts and push; the check runs again on the result."
  echo "::error::${message}"
  echo "$message" >> "${GITHUB_STEP_SUMMARY:-/dev/null}"
  exit 1
fi
echo "Now testing $(git rev-parse HEAD | cut -c1-12), which is the pull request on top of the live ${base}."
