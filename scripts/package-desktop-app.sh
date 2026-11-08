#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
  echo 'Usage: sh scripts/package-desktop-app.sh RELEASE_ZIP OUTPUT_DIRECTORY' >&2
  exit 1
fi
archive=$1
output=$2
[ -f "$archive" ] && [ -f "$archive.sha256" ] || { echo 'Release ZIP and .sha256 are required.' >&2; exit 1; }
installer=$(/usr/bin/dirname "$archive")/install-desktop.sh
[ -f "$installer" ] || { echo 'Matching install-desktop.sh is required beside the release ZIP.' >&2; exit 1; }
expected=$(/usr/bin/awk 'NR == 1 { print $1 }' < "$archive.sha256")
actual=$(/usr/bin/shasum -a 256 < "$archive" | /usr/bin/awk '{print $1}')
[ "${#expected}" -eq 64 ] && [ "$expected" = "$actual" ] || { echo 'Release checksum mismatch.' >&2; exit 1; }
name=$(/usr/bin/basename "$archive")
case $name in
  opencut-plugin-*-x86_64-apple-darwin.zip) target=x86_64-apple-darwin ;;
  opencut-plugin-*-aarch64-apple-darwin.zip) target=aarch64-apple-darwin ;;
  *) echo 'Expected an official macOS OpenCut platform ZIP.' >&2; exit 1 ;;
esac
host=$(/usr/bin/uname -m)
case "$target:$host" in
  x86_64-apple-darwin:x86_64|aarch64-apple-darwin:arm64) ;;
  *) echo "Package $target on its native Mac (found $host)." >&2; exit 1 ;;
esac
release=$(/usr/bin/unzip -p "$archive" opencut/release.json)
[ -n "$release" ] || { echo 'Missing release metadata.' >&2; exit 1; }
version=$(printf '%s' "$release" | /usr/bin/plutil -extract version raw -o - -)
declared_target=$(printf '%s' "$release" | /usr/bin/plutil -extract target raw -o - -)
[ "$target" = "$declared_target" ] || { echo 'Release target differs from ZIP name.' >&2; exit 1; }
work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/opencut-app.XXXXXX")
trap '/bin/rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM
app="$work/OpenCut.app"
/bin/mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
export CLANG_MODULE_CACHE_PATH="$work/clang-cache"
/usr/bin/xcrun swiftc -O -module-cache-path "$work/swift-cache" -target "$host-apple-macosx12.0" \
  -file-prefix-map "$HOME=/opencut-build/user" -file-prefix-map "$PWD=/opencut-build/source" \
  -debug-prefix-map "$HOME=/opencut-build/user" -debug-prefix-map "$work=/opencut-build/output" \
  -framework AppKit -framework Metal apps/desktop-launcher/main.swift \
  -o "$app/Contents/MacOS/OpenCut"
/bin/cp "$installer" "$app/Contents/Resources/install-desktop.sh"
/bin/cp "$archive" "$app/Contents/Resources/opencut-release.zip"
/bin/cp "$archive.sha256" "$app/Contents/Resources/opencut-release.zip.sha256"
/bin/cp apps/desktop-launcher/assets/OpenCut.icns "$app/Contents/Resources/OpenCut.icns"
/bin/cp apps/desktop-launcher/switch-workspace.mjs "$app/Contents/Resources/switch-workspace.mjs"
/bin/cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>io.opencut.desktop</string>
  <key>CFBundleExecutable</key><string>OpenCut</string>
  <key>CFBundleName</key><string>OpenCut</string>
  <key>CFBundleDisplayName</key><string>OpenCut</string>
  <key>CFBundleIconFile</key><string>OpenCut.icns</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
/usr/bin/plutil -lint "$app/Contents/Info.plist" >/dev/null
# Ad-hoc signing makes this local build runnable without an Apple Developer ID.
# It does not satisfy Gatekeeper for downloaded apps.
/usr/bin/codesign --force --sign - "$app"
/bin/mkdir -p "$output"
destination="$output/OpenCut-$version-$target.app"
[ ! -e "$destination" ] || { echo "Destination exists: $destination" >&2; exit 1; }
/bin/mv "$app" "$destination"
bundle_zip="$output/OpenCut-$version-$target.zip"
[ ! -e "$bundle_zip" ] || { echo "Destination exists: $bundle_zip" >&2; exit 1; }
/usr/bin/ditto -c -k --keepParent "$destination" "$bundle_zip"
digest=$(/usr/bin/shasum -a 256 < "$bundle_zip" | /usr/bin/awk '{ print $1 }')
printf '%s  %s\n' "$digest" "$(/usr/bin/basename "$bundle_zip")" > "$bundle_zip.sha256"
echo "$destination"
