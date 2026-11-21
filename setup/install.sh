#!/usr/bin/env bash
# Downloads one release of embr for this runner and puts it on PATH.
# setup/action.yml runs it. It reads:
#
#   EMBR_VERSION      a version such as 1.0.0, or `latest` for the newest
#                     stable release
#   EMBR_PROVENANCE   `true` to also check the build provenance of the
#                     download with `gh attestation verify`
#   EMBR_REPOSITORY   the repository that publishes embr
#   RUNNER_OS, RUNNER_ARCH, RUNNER_TEMP, GITHUB_PATH, GITHUB_OUTPUT
#                     as GitHub Actions sets them
#
# The download is checked against the SHA256SUMS file of the release. That
# file and the binary come from the same release, so the check catches a bad
# transfer, not a release that somebody replaced. EMBR_PROVENANCE is the
# check for that.
set -euo pipefail

repository="${EMBR_REPOSITORY:-ember-luau/ember}"
version="${EMBR_VERSION:-latest}"
base="https://github.com/$repository/releases"

case "${RUNNER_OS:-$(uname -s)}" in
  Linux) os=linux ;;
  macOS | Darwin) os=macos ;;
  Windows | MINGW* | MSYS*) os=windows ;;
  *) echo "::error::embr has no release for the system ${RUNNER_OS:-$(uname -s)}" >&2; exit 1 ;;
esac
case "${RUNNER_ARCH:-$(uname -m)}" in
  X64 | x86_64 | amd64) arch=x86_64 ;;
  ARM64 | arm64 | aarch64) arch=aarch64 ;;
  *) echo "::error::embr has no release for the processor ${RUNNER_ARCH:-$(uname -m)}" >&2; exit 1 ;;
esac
# The releases hold these four builds. An x86_64 build runs on a Windows
# ARM64 runner, and nothing else stands in for another.
if [ "$os" = windows ] && [ "$arch" = aarch64 ]; then arch=x86_64; fi
case "$os-$arch" in
  linux-x86_64 | macos-x86_64 | macos-aarch64 | windows-x86_64) ;;
  *) echo "::error::embr has no release for $os-$arch; build it with \`cargo install embr\`" >&2; exit 1 ;;
esac

asset="embr-$os-$arch.zip"

if [ "$version" = latest ]; then
  # /releases/latest/download/<asset> answers with a redirect to the asset
  # of the newest stable release, and the tag is in that address. So this
  # asks no API and meets no rate limit.
  location="$(curl -fsSI "$base/latest/download/$asset" | tr -d '\r' | awk 'tolower($1) == "location:" { print $2 }' | head -n 1)"
  version="${location##*/download/v}"
  version="${version%%/*}"
  if [ -z "$location" ] || [ -z "$version" ]; then
    echo "::error::could not read the latest release of $repository" >&2
    exit 1
  fi
fi
version="${version#v}"
# The version goes into a URL and a folder name.
case "$version" in
  '' | *[!0-9A-Za-z.+-]*) echo "::error::'$version' is not a version of embr" >&2; exit 1 ;;
esac

work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/embr-setup.XXXXXX")"
trap 'rm -rf "$work"' EXIT

echo "Downloading embr $version ($asset)"
curl -fsSL --retry 3 -o "$work/$asset" "$base/download/v$version/$asset"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

if curl -fsSL --retry 3 -o "$work/SHA256SUMS" "$base/download/v$version/SHA256SUMS" 2>/dev/null; then
  expected="$(awk -v name="$asset" '{ file = $2; sub(/^\*/, "", file) } file == name { print $1 }' "$work/SHA256SUMS")"
  received="$(sha256 "$work/$asset")"
  if [ -z "$expected" ]; then
    echo "::error::the SHA256SUMS of embr $version does not list $asset" >&2
    exit 1
  fi
  if [ "$expected" != "$received" ]; then
    echo "::error::$asset does not match the SHA256SUMS of the release: expected $expected, received $received" >&2
    exit 1
  fi
  echo "The sha256 matches the SHA256SUMS of the release"
else
  echo "::warning::embr $version has no SHA256SUMS file, so the download was not checked"
fi

if [ "${EMBR_PROVENANCE:-false}" = true ]; then
  # Fails for a release from before release.yml attested its builds.
  gh attestation verify "$work/$asset" --repo "$repository"
fi

install="${RUNNER_TEMP:-$work/..}/embr-$version"
mkdir -p "$install"
unzip -q -o "$work/$asset" -d "$install"
binary="embr"
if [ "$os" = windows ]; then binary="embr.exe"; fi
if [ ! -f "$install/$binary" ]; then
  echo "::error::$asset holds no $binary" >&2
  exit 1
fi
chmod +x "$install/$binary"

# embr first, then the folder of the tool shims that `embr install` writes.
if [ -n "${GITHUB_PATH:-}" ]; then
  if [ "$os" = windows ] && command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$install" >> "$GITHUB_PATH"
    cygpath -w "$HOME/.ember/bin" >> "$GITHUB_PATH"
  else
    echo "$install" >> "$GITHUB_PATH"
    echo "$HOME/.ember/bin" >> "$GITHUB_PATH"
  fi
fi
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "version=$version" >> "$GITHUB_OUTPUT"
  echo "path=$install" >> "$GITHUB_OUTPUT"
fi
"$install/$binary" --version
