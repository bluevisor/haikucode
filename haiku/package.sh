#!/bin/bash
# Build Haiku Code and wrap it in an .hpkg. Run ON Haiku from anywhere:
#   haiku/package.sh             -> dist/haikucode-<version>-<rev>-<arch>.hpkg
#   HAIKUCODE_PKG_REV=2 haiku/package.sh   bump the package revision
# Install by copying the .hpkg into ~/config/packages (activates instantly,
# no reboot) or with `pkgman install <file>`.
set -eo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
rev=${HAIKUCODE_PKG_REV:-1}
arch=$(getarch)
out="dist/haikucode-$version-$rev-$arch.hpkg"

cargo build --release --locked

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin" "$stage/apps" "$stage/data/deskbar/menu/Applications" \
	"$stage/documentation/packages/haikucode"

install -m 755 target/release/haikucode "$stage/bin/haikucode"

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

sed -e "s/@VERSION@/$version/g" -e "s/@REVISION@/$rev/g" -e "s/@ARCH@/$arch/g" \
	haiku/PackageInfo.in > "$stage/.PackageInfo"

mkdir -p dist
rm -f "$out"
package create -C "$stage" "$out"
echo "$out"
