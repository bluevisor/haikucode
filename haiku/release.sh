#!/usr/bin/env bash
# Publish a GitHub release for Cargo.toml's version, carrying the Haiku package
# that `haikucode update` / `/update` installs. main is protected, so land the
# version bump through a PR first, then run this from an up-to-date main:
#   haiku/release.sh
# Builds arm64 on the Haiku guest via deploy.sh (HAIKU_HOST, default haiku-dbg)
# and cross-builds x86_64 and 32-bit x86 with haiku/cross.sh (Docker).
# Pushes as the bluevisor GitHub account unless GH_TOKEN is set.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
tag="v$version"
[[ -z $(git status --porcelain) ]] || { echo "!! commit your changes first" >&2; exit 1; }
git fetch -q origin main
[[ $(git rev-parse HEAD) == $(git rev-parse origin/main) ]] || { echo "!! check out main at origin/main (merge the release PR, then pull)" >&2; exit 1; }

token=${GH_TOKEN:-$(gh auth token --user bluevisor)}
if GH_TOKEN=$token gh release view "$tag" -R bluevisor/haikucode >/dev/null 2>&1; then
	echo "!! $tag is already released; bump the version in Cargo.toml" >&2
	exit 1
fi

haiku/deploy.sh fetch
haiku/cross.sh x86_64
haiku/cross.sh x86
pkgs=()
for a in arm64 x86_64 x86_gcc2; do
	pkgs+=("$(ls -t dist/haikucode-"$version"-*-"$a".hpkg | head -1)")
done
notes=$(git log -1 --format=%b | grep -v '^Co-Authored-By:')

GH_TOKEN=$token gh release create "$tag" "${pkgs[@]}" -R bluevisor/haikucode \
	--target "$(git rev-parse HEAD)" --title "Haiku Code $version" --notes "$notes"
