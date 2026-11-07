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
# Through pkgman, not by copying the file into ~/config/packages: a dropped-in
# file mounts at once but may never reach administrative/activated-packages,
# and after a reboot packagefs leaves it inactive (`haikucode: command not
# found` with the .hpkg still sitting there). pkgman replaces the installed
# version, older or newer, in one transaction.
name=$(basename "$pkg")
# pkgman can't replace a package with the same version ("Name in use"): skip
# the install when that exact build is already active, else uninstall first.
ssh "$host" "if grep -qx '$name' ~/config/packages/administrative/activated-packages && cmp -s ~/config/bin/haikucode $src/target/release/haikucode; then
		echo '>> $name is already installed'
	else
		if grep -qx '$name' ~/config/packages/administrative/activated-packages; then
			pkgman uninstall -H -y haikucode > /tmp/haikucode-pkgman.log 2>&1 || { cat /tmp/haikucode-pkgman.log >&2; exit 1; }
		fi
		pkgman install -H -y $src/$pkg > /tmp/haikucode-pkgman.log 2>&1 || { cat /tmp/haikucode-pkgman.log >&2; exit 1; }
	fi
	grep -qx '$name' ~/config/packages/administrative/activated-packages || { echo '!! $name is not in activated-packages' >&2; exit 1; }
	for i in \$(seq 40); do cmp -s ~/config/bin/haikucode $src/target/release/haikucode && break; sleep 0.25; done
	cmp -s ~/config/bin/haikucode $src/target/release/haikucode || { echo '!! installed binary differs from the build' >&2; exit 1; }
	# Flush to disk: BFS journals only metadata, so if the VM stops before the
	# data is written back, the .hpkg keeps its size but reads as all zeros and
	# packagefs skips it (\`haikucode: command not found\` after the restart).
	sync
	haikucode --version"

if [[ "${1:-}" == "fetch" ]]; then
	mkdir -p dist
	scp -q "$host:$src/$pkg" dist/
	echo ">> dist/$(basename "$pkg")"
fi
