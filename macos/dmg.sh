#!/usr/bin/env bash
# macOS disk images of Haiku Code, run on a Mac:
#   macos/dmg.sh   -> dist/haikucode-<version>-macos-{arm64,x86_64}.dmg
# Each holds the haikucode binary and "Install Haiku Code.command", which
# copies it to ~/.local/bin like install.sh. The binary is signed with this
# Mac's Developer ID (HAIKUCODE_SIGN picks another identity) with the
# hardened runtime, and each DMG is notarized and stapled with the notarytool
# keychain profile $HAIKUCODE_NOTARY (default terminal-cam-notary;
# HAIKUCODE_NOTARY=none skips), so Gatekeeper opens it without "Open Anyway".
# The release's .tar.gz files (built in GitHub Actions) stay: install.sh and
# `haikucode update` use those.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
sign=${HAIKUCODE_SIGN:-$(security find-identity -v -p codesigning \
	| grep -oE '"Developer ID Application: [^"]*"' | head -1 | tr -d '"')}
[ -n "$sign" ] || { echo "!! no Developer ID Application identity in the keychain" >&2; exit 1; }
notary=${HAIKUCODE_NOTARY:-terminal-cam-notary}

mkdir -p dist
for pair in aarch64-apple-darwin:arm64 x86_64-apple-darwin:x86_64; do
	target=${pair%%:*}
	arch=${pair##*:}
	echo ">> macos-$arch"
	HAIKUCODE_ASSET=macos-$arch cargo build --release --locked --target "$target" 2>&1 \
		| grep -Ev '^\s+(Compiling|Downloaded|Downloading)' || true
	stage=$(mktemp -d)
	cp "target/$target/release/haikucode" "$stage/haikucode"
	codesign --force --options runtime --timestamp -s "$sign" "$stage/haikucode"
	cat > "$stage/Install Haiku Code.command" <<'SCRIPT'
#!/bin/sh
# Copies haikucode from this disk image to ~/.local/bin (or $HAIKUCODE_DIR),
# as install.sh does. After that, `haikucode update` keeps it current.
here=$(cd "$(dirname "$0")" && pwd)
dir=${HAIKUCODE_DIR:-$HOME/.local/bin}
mkdir -p "$dir" || exit 1
cp "$here/haikucode" "$dir/haikucode.new.$$" && chmod 755 "$dir/haikucode.new.$$" \
	&& mv "$dir/haikucode.new.$$" "$dir/haikucode" || exit 1
echo "Installed $("$dir/haikucode" --version) to $dir/haikucode"
case ":$PATH:" in
*":$dir:"*) echo "Run it in any terminal: haikucode" ;;
*)
	rc="$HOME/.zshrc"
	case "${SHELL:-}" in */bash) rc="$HOME/.bash_profile" ;; esac
	echo "export PATH=\"$dir:\$PATH\"" >> "$rc"
	echo "Added $dir to your PATH in $rc; open a new terminal and run: haikucode"
	;;
esac
SCRIPT
	chmod 755 "$stage/Install Haiku Code.command"
	cp LICENSE "$stage/LICENSE.txt"

	dmg="dist/haikucode-$version-macos-$arch.dmg"
	# A read-write pass first: the icon Finder shows for the mounted image is a
	# .VolumeIcon.icns on it plus the volume's custom-icon bit, and a UDZO image
	# is read-only, so the icon has to be in place before the final conversion.
	rw="$(mktemp -d)/HaikuCode.dmg"
	hdiutil create -quiet -ov -volname "Haiku Code $version" -srcfolder "$stage" -format UDRW "$rw"
	if command -v SetFile >/dev/null; then
		mnt=$(hdiutil attach -readwrite -noverify -noautoopen "$rw" | grep -o '/Volumes/.*' | tail -1)
		cp assets/haikucode.icns "$mnt/.VolumeIcon.icns"
		SetFile -a V "$mnt/.VolumeIcon.icns"
		SetFile -a C "$mnt"
		sync
		# Spotlight can still be indexing the fresh volume: give it a moment.
		hdiutil detach -quiet "$mnt" || { sleep 2; hdiutil detach -quiet "$mnt"; }
	else
		echo "   (no SetFile: the disk image gets no icon)" >&2
	fi
	hdiutil convert "$rw" -ov -format UDZO -o "$dmg"
	rm -rf "$(dirname "$rw")"
	rm -rf "${stage:?}"
	codesign --force --timestamp -s "$sign" "$dmg"
	if [ "$notary" != none ]; then
		xcrun notarytool submit "$dmg" --keychain-profile "$notary" --wait
		xcrun stapler staple "$dmg"
	fi
	echo ">> $dmg"
done
