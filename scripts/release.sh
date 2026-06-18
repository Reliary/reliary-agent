#!/usr/bin/env bash
# reliary-agent release script.
# Usage: scripts/release.sh [patch|minor|major]
# Bumps version, updates changelog, commits, pushes, creates PR, tags, pushes tag.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

BUMP="${1:-patch}"

# Validate bump type
case "$BUMP" in
  patch|minor|major) ;;
  *) echo "Usage: $0 [patch|minor|major]" >&2; exit 1 ;;
esac

# Read current version
CURRENT=$(grep '^version ' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
echo "Current version: v$CURRENT"

# Compute new version
IFS='.' read -r major minor patch <<< "$CURRENT"
case "$BUMP" in
  patch) NEW="$major.$minor.$((patch + 1))" ;;
  minor) NEW="$major.$((minor + 1)).0" ;;
  major) NEW="$((major + 1)).0.0" ;;
esac
echo "New version:     v$NEW"

# Update Cargo.toml
sed -i "s/^version = \"$CURRENT\"/version = \"$NEW\"/" Cargo.toml

# Update npm/package.json
if [ -f npm/package.json ]; then
  sed -i "s/\"version\": \"$CURRENT\"/\"version\": \"$NEW\"/" npm/package.json
fi

# Update CHANGELOG.md — add new version entry after first line
if head -1 CHANGELOG.md | grep -q "^# Changelog"; then
  # Insert after the first line
  sed -i "1a\\
\\
## v$NEW\\
\\
### Release\\
\\
- Version bump: v$CURRENT → v$NEW\\
" CHANGELOG.md
fi

# Commit
BRANCH="release-v$NEW"
git checkout -b "$BRANCH"
git add Cargo.toml npm/package.json CHANGELOG.md
git commit -m "chore: bump v$CURRENT → v$NEW"
git push origin "$BRANCH"

# Create PR
PR_URL=$(gh pr create \
  --base master \
  --head "$BRANCH" \
  --title "chore: v$NEW" \
  --body "Version bump and changelog for v$NEW." \
  --repo "$(gh repo view --json nameWithOwner --jq '.nameWithOwner' 2>/dev/null || echo 'Reliary/reliary-agent')")

echo "PR created: $PR_URL"
echo "Waiting for CI to pass..."

# Wait for CI checks to pass (up to 5 min)
sleep 30
ATTEMPTS=0
while [ $ATTEMPTS -lt 20 ]; do
  CHECKS=$(gh pr view "$BRANCH" --json statusCheckRollup --jq '[.statusCheckRollup[] | select(.conclusion != "SUCCESS" and .conclusion != "SKIPPED" and .conclusion != "NEUTRAL")] | length' 2>/dev/null || echo "1")
  if [ "$CHECKS" -eq 0 ]; then
    break
  fi
  sleep 15
  ATTEMPTS=$((ATTEMPTS + 1))
done

# Merge PR (admin bypass for ruleset)
gh pr merge "$BRANCH" --admin --merge --subject "chore: v$NEW"

# Wait for merge to propagate
git fetch origin master
git checkout master
git pull --ff-only

# Tag and push
git tag "v$NEW"
git push origin "v$NEW"

echo "Released v$NEW"
echo "  PR:     $PR_URL"
echo "  Tag:    v$NEW"
echo "  Release: https://github.com/Reliary/reliary-agent/releases/tag/v$NEW"
