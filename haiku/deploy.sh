#!/usr/bin/env bash
# Build Haiku Code on a Haiku machine and install it there, from the Mac.
#   haiku/deploy.sh          ship the source, build + package on the guest,
#                            activate the .hpkg in ~/config/packages
#   haiku/deploy.sh fetch    also copy the .hpkg back into ./dist
# HAIKU_HOST is an ssh destination (default haiku-dbg: the UTM VM's
# 127.0.0.1:2222 port forward; use `haiku` when it's on vmnet at 192.168.64.4).
# Rust can't cross-compile to Haiku from macOS here, so the guest builds
# natively with its own rustc/cargo.
set -eo pipefail
cd "$(dirname "$0")/.."
host=${HAIKU_HOST:-haiku-dbg}
src='~/src/haikucode'

echo ">> syncing source to $host:$src"
COPYFILE_DISABLE=1 tar czf - --no-xattrs --exclude ./target --exclude ./dist \
	--exclude ./.git --exclude .DS_Store . \
	| ssh "$host" "mkdir -p $src && cd $src && tar xzf -"

echo ">> building + packaging on $host"
pkg=$(ssh "$host" "cd $src && haiku/package.sh" | tail -1)

echo ">> activating $pkg"
# Packages in ~/config/packages activate (and deactivate) a moment after the
# file lands (or goes). Wait out each step: swapping a same-version package in
# one go races the package daemon and can leave the old contents mounted.
ssh "$host" "rm -f ~/config/packages/haikucode-*.hpkg
	for i in \$(seq 40); do [ -e ~/config/bin/haikucode ] || break; sleep 0.25; done
	cp $src/$pkg ~/config/packages/
	for i in \$(seq 40); do [ -e ~/config/bin/haikucode ] && break; sleep 0.25; done
	cmp -s ~/config/bin/haikucode $src/target/release/haikucode || { echo '!! installed binary differs from the build' >&2; exit 1; }
	haikucode --version"

if [[ "${1:-}" == "fetch" ]]; then
	mkdir -p dist
	scp -q "$host:$src/$pkg" dist/
	echo ">> dist/$(basename "$pkg")"
fi
