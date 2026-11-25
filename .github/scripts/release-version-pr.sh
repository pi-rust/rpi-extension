#!/usr/bin/env bash
# Merge only the tested release metadata through a PR, never push to main.
set -euo pipefail

base="$(git rev-parse HEAD)"
# Bounded wait so a slow or failed required check fails the run instead of
# racing the merge. Overridable for tests.
REQUIRED_CHECK="${RELEASE_REQUIRED_CHECK:-require-issue}"
CHECK_TIMEOUT="${RELEASE_CHECK_TIMEOUT_SECONDS:-1200}"
CHECK_INTERVAL="${RELEASE_CHECK_INTERVAL_SECONDS:-10}"

# Poll one required check until it succeeds, fails, or the deadline passes.
wait_for_required_checks() {
  local pr="$1" deadline state
  deadline=$(( $(date +%s) + CHECK_TIMEOUT ))
  while true; do
    # A running check has no conclusion yet; a missing entry is pending too.
    state="$(gh pr view "$pr" --json statusCheckRollup --jq \
      "([.statusCheckRollup[] | select((.name // .context) == \"$REQUIRED_CHECK\")] | first | (.conclusion // .state // \"PENDING\")) // \"PENDING\"")"
    case "$state" in
      SUCCESS)
        return 0
        ;;
      FAILURE|CANCELLED|TIMED_OUT|ACTION_REQUIRED|STARTUP_FAILURE|SKIPPED|STALE)
        echo "::error::Required check $REQUIRED_CHECK ended as $state on the version PR. Rerun the release workflow."
        return 1
        ;;
    esac
    if (( $(date +%s) >= deadline )); then
      echo "::error::Timed out after ${CHECK_TIMEOUT}s waiting for required check $REQUIRED_CHECK on the version PR."
      return 1
    fi
    sleep "$CHECK_INTERVAL"
  done
}
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git add Cargo.toml Cargo.lock packages/ catalog/packages.json
if ! git diff --cached --quiet; then
  branch="codex/release-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"
  git switch -c "$branch"
  git commit -m "chore(release): update versions and changelogs"
  expected_tree="$(git rev-parse 'HEAD^{tree}')"
  if [[ "$(git ls-remote origin refs/heads/main | cut -f1)" != "$base" ]]; then
    echo "::error::main advanced during validation. Retry the release on the new main."
    exit 1
  fi
  git push origin "HEAD:refs/heads/$branch"
  body="${RUNNER_TEMP}/release-pr-body.md"
  cat > "$body" <<'EOF'
Updates extension versions, changelogs, Cargo.lock and the package catalog for the automatic release.

Validation: workspace tests and Cargo publishing dry-run passed before this PR was created.
EOF
  pr="$(gh pr create --base main --head "$branch" \
    --title 'chore(release): update versions and changelogs' --body-file "$body")"
  if [[ "$(git ls-remote origin refs/heads/main | cut -f1)" != "$base" ]]; then
    echo "::error::main advanced before the version PR could merge. Retry the release."
    exit 1
  fi
  # Branch protection requires `require-issue`, which starts after the PR exists.
  # Merging immediately races that check and GitHub rejects the merge as blocked.
  wait_for_required_checks "$pr"
  gh pr merge "$pr" --squash --match-head-commit "$(git rev-parse HEAD)"
  merged="$(gh pr view "$pr" --json mergeCommit --jq .mergeCommit.oid)"
  git fetch origin "$merged"
  git checkout --detach "$merged"
  if [[ "$(git rev-parse 'HEAD^{tree}')" != "$expected_tree" ]]; then
    echo "::error::The merged tree changed during validation. Retry before publishing."
    exit 1
  fi
  git push origin --delete "$branch"
fi

echo "source_ref=$(git rev-parse HEAD)" >> "$GITHUB_OUTPUT"
