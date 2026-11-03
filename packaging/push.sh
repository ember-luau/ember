#!/usr/bin/env bash
# Commits one written file to a GitHub repo and pushes it to main.
#
#   TOKEN=... VERSION=0.14.0 packaging/push.sh OWNER/REPO PATH FILE
#
# PATH is where FILE goes in the repo. Without TOKEN the script shows a
# notice and stops, because the channel is not set up yet. When the repo
# already holds the same file, the script makes no commit.
set -euo pipefail

repo=$1
dest=$2
file=$(realpath "$3")
if [ -z "${TOKEN:-}" ]; then
  echo "::notice::The token for $repo is not set, so $dest was written and checked but not pushed."
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git clone -q "https://x-access-token:$TOKEN@github.com/$repo.git" "$work"
cd "$work"
# The commit goes on main, also when the default branch of the repo is
# another one. A new empty repo has no main yet, and this commit makes it.
if git rev-parse -q --verify origin/main >/dev/null; then
  git checkout -q -B main origin/main
fi
mkdir -p "$(dirname "$dest")"
cp "$file" "$dest"
git add "$dest"
if git diff --cached --quiet; then
  echo "::notice::$repo already has $dest for embr $VERSION."
  exit 0
fi
git -c user.name=savruun -c user.email=savruun@users.noreply.github.com commit -q -m "embr $VERSION"
git push -q origin HEAD:main
echo "pushed $dest for embr $VERSION to $repo"
