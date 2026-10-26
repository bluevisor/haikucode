#!/bin/bash
# Build Haiku Code and wrap it in an .hpkg. Run ON Haiku from anywhere:
#   haiku/package.sh             -> dist/haikucode-<version>-<rev>-<arch>.hpkg
#   HAIKUCODE_PKG_REV=2 haiku/package.sh   bump the package revision
#   HAIKUCODE_BIN=haikucode HAIKUCODE_ARCH=x86_64 haiku/package.sh
#                                package a cross-built binary (haiku/cross.sh)
# Install by copying the .hpkg into ~/config/packages (activates instantly,
# no reboot) or with `pkgman install <file>`.
set -eo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rev=${HAIKUCODE_PKG_REV:-1}
arch=${HAIKUCODE_ARCH:-$(getarch)}
bin=${HAIKUCODE_BIN:-target/release/haikucode}
out="dist/haikucode-$version-$rev-$arch.hpkg"

[ -n "$HAIKUCODE_BIN" ] || cargo build --release --locked

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin" "$stage/apps" "$stage/data/deskbar/menu/Applications" \
	"$stage/documentation/packages/haikucode"

install -m 755 "$bin" "$stage/bin/haikucode"
ln -s haikucode "$stage/bin/hc"

app="$stage/apps/Haiku Code"
install -m 755 haiku/launcher.sh "$app"
# An executable type makes Deskbar/Tracker run the script instead of opening
# it in an editor. The icon is Terminal's, since that's the window it opens.
settype -t application/x-vnd.Be-elfexecutable "$app"
catattr -r BEOS:ICON /boot/system/apps/Terminal > "$stage/icon"
addattr -f "$stage/icon" -c VICN BEOS:ICON "$app"
rm "$stage/icon"
ln -s "../../../../apps/Haiku Code" "$stage/data/deskbar/menu/Applications/Haiku Code"

cp README.md LICENSE "$stage/documentation/packages/haikucode/"

# 32-bit Haiku (x86_gcc2h) runs a modern-gcc binary against its secondary
# x86 libraries, so require those (haiku_x86, lib:…_x86) as HaikuPorts does.
secondary=()
[ "$arch" = x86_gcc2 ] && secondary=(-e 's/^\thaiku$/\thaiku\n\thaiku_x86/' -e 's/^\t\(lib:[a-z_+]*\)$/\t\1_x86/')
sed -e "s/@VERSION@/$version/g" -e "s/@REVISION@/$rev/g" -e "s/@ARCH@/$arch/g" "${secondary[@]}" \
	haiku/PackageInfo.in > "$stage/.PackageInfo"

mkdir -p dist
rm -f "$out"
package create -C "$stage" "$out"
echo "$out"
