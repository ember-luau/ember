#!/usr/bin/env bash
# Writes the Homebrew formula, the Scoop manifest and the winget manifests
# of one release from the templates in packaging/.
#
#   packaging/render.sh VERSION SHA256SUMS OUT
#
# VERSION can start with a v. SHA256SUMS is the checksum file of the
# release. OUT gets embr.rb, embr.json and winget/ with the three winget
# manifests. RELEASE_DATE (YYYY-MM-DD) sets the winget release date, else
# today in UTC. The script writes files only and pushes nothing.
set -euo pipefail

version=${1#v}
sums=$2
out=$3
here=$(dirname "$0")
date=${RELEASE_DATE:-$(date -u +%F)}

# The sum of one asset. A missing asset stops the script, so no channel
# gets a formula with an empty hash.
sum() {
  awk -v name="$1" '$2 == name || $2 == "*" name { print $1 }' "$sums"
}
linux=$(sum embr-linux-x86_64.zip)
macos_arm=$(sum embr-macos-aarch64.zip)
macos_intel=$(sum embr-macos-x86_64.zip)
windows=$(sum embr-windows-x86_64.zip)
for hash in "$linux" "$macos_arm" "$macos_intel" "$windows"; do
  if [[ ! "$hash" =~ ^[0-9a-f]{64}$ ]]; then
    echo "a release zip has no SHA-256 in $sums" >&2
    exit 1
  fi
done

fill() {
  sed -e "s/@VERSION@/$version/g" -e "s/@RELEASE_DATE@/$date/g" \
    -e "s/@SHA256_LINUX_X86_64@/$linux/g" -e "s/@SHA256_MACOS_AARCH64@/$macos_arm/g" \
    -e "s/@SHA256_MACOS_X86_64@/$macos_intel/g" -e "s/@SHA256_WINDOWS_X86_64@/$windows/g" \
    "$1" >"$2"
}

mkdir -p "$out/winget"
fill "$here/homebrew/embr.rb" "$out/embr.rb"
fill "$here/scoop/embr.json" "$out/embr.json"
for manifest in "$here"/winget/*.yaml; do
  fill "$manifest" "$out/winget/$(basename "$manifest")"
done

if grep -rn '@[A-Z0-9_]*@' "$out"; then
  echo "a placeholder above has no value" >&2
  exit 1
fi
echo "wrote embr $version to $out"
