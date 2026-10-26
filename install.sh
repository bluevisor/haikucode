#!/bin/sh
# Install Haiku Code on macOS or Linux:
#   curl -fsSL https://raw.githubusercontent.com/bluevisor/haikucode/main/install.sh | sh
# Puts the latest release's binary for this machine in ~/.local/bin (or
# $HAIKUCODE_DIR). After that, `haikucode update` keeps it current.
# On Haiku, install the package instead: `haikucode update`, or see the README.
set -eu

case "$(uname -s)" in
Darwin) os=macos ;;
Linux) os=linux ;;
*) echo "haikucode: no release build for $(uname -s); build from source (cargo build --release)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
arm64 | aarch64) arch=arm64 ;;
x86_64 | amd64) arch=x86_64 ;;
armv7l) arch=armv7 ;;
armv6l) arch=armv6 ;;
*) echo "haikucode: no release build for $(uname -m); build from source (cargo build --release)" >&2; exit 1 ;;
esac
[ "$os" = macos ] && case $arch in arm64 | x86_64) ;; *) echo "haikucode: no macOS build for $arch" >&2; exit 1 ;; esac

dir=${HAIKUCODE_DIR:-$HOME/.local/bin}
api=https://api.github.com/repos/bluevisor/haikucode/releases/latest
url=$(curl -fsSL "$api" | grep -o "https://[^\"]*/haikucode-[^\"]*-$os-$arch\.tar\.gz" | head -1)
[ -n "$url" ] || { echo "haikucode: the latest release has no $os-$arch build yet" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "downloading ${url##*/}…"
curl -fsSL "$url" | tar -xzf - -C "$tmp" haikucode
mkdir -p "$dir"
mv "$tmp/haikucode" "$dir/haikucode.new.$$"
chmod 755 "$dir/haikucode.new.$$"
mv "$dir/haikucode.new.$$" "$dir/haikucode"
echo "installed $("$dir/haikucode" --version) to $dir/haikucode"
case ":$PATH:" in *":$dir:"*) ;; *) echo "add $dir to your PATH to run it as haikucode" ;; esac
