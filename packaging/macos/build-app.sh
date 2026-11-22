#!/usr/bin/env bash
# Build Halyard.app: a universal (arm64 + x86_64) macOS app bundle, ad-hoc signed, plus a zip.
#
# Usage: packaging/macos/build-app.sh [--out DIR] [--skip-build]
#   --out DIR      where to write Halyard.app and the zip (default: <repo>/target/macos)
#   --skip-build   reuse the release binaries already in target/<triple>/release
#
# Needs: Rust (rustup, with both apple-darwin targets), Zig 0.15.2 on PATH, and the tools
# that ship with macOS: lipo, sips, iconutil, codesign, ditto, plutil.
# The icon comes from packaging/icon/halyard-1024.png (rendered from halyard.svg; see
# packaging/icon/README.md). No other tool is needed to build the bundle.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
out="$root/target/macos"
skip_build=0
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="${2:?--out needs a directory}"; shift 2 ;;
    --skip-build) skip_build=1; shift ;;
    -h | --help) sed -n '2,12p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ "$(uname -s)" != "Darwin" ]; then
  echo "build-app.sh only runs on macOS" >&2
  exit 1
fi

# Version comes from the workspace Cargo.toml. A "-dev" or other pre-release suffix is dropped
# because CFBundleShortVersionString must be numeric dotted.
full_version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)"
if [ -z "$full_version" ]; then
  echo "could not read the workspace version from Cargo.toml" >&2
  exit 1
fi
version="${full_version%%-*}"
min_macos="13.0"
bundle_id="io.github.pete-builds.Halyard"
targets=(aarch64-apple-darwin x86_64-apple-darwin)

target_dir="${CARGO_TARGET_DIR:-$root/target}"

if [ "$skip_build" -eq 0 ]; then
  for t in "${targets[@]}"; do
    echo "==> cargo build --release --target $t"
    MACOSX_DEPLOYMENT_TARGET="$min_macos" \
      cargo build --release --locked -p halyard-app --target "$t" --manifest-path "$root/Cargo.toml"
  done
fi

app="$out/Halyard.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

echo "==> lipo"
lipo -create \
  "$target_dir/aarch64-apple-darwin/release/halyard" \
  "$target_dir/x86_64-apple-darwin/release/halyard" \
  -output "$app/Contents/MacOS/halyard"
chmod 755 "$app/Contents/MacOS/halyard"

echo "==> icon"
icon_src="$root/packaging/icon/halyard-1024.png"
iconset="$(mktemp -d)/Halyard.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  double=$((size * 2))
  sips -z "$size" "$size" "$icon_src" --out "$iconset/icon_${size}x${size}.png" > /dev/null
  sips -z "$double" "$double" "$icon_src" --out "$iconset/icon_${size}x${size}@2x.png" > /dev/null
done
iconutil --convert icns "$iconset" --output "$app/Contents/Resources/Halyard.icns"
rm -rf "$(dirname "$iconset")"

cat > "$app/Contents/Info.plist" << PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>halyard</string>
  <key>CFBundleIconFile</key><string>Halyard</string>
  <key>CFBundleIdentifier</key><string>${bundle_id}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Halyard</string>
  <key>CFBundleDisplayName</key><string>Halyard</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>${min_macos}</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"

# Ad-hoc signature ("-"): lets the bundle launch on the machine that built it and keeps the
# bundle internally consistent. It is NOT a Developer ID signature and the app is NOT
# notarized, so Gatekeeper warns on a downloaded copy. See docs/INSTALL.md.
echo "==> codesign (ad-hoc)"
codesign --force --sign - --timestamp=none "$app"
codesign --verify --strict --verbose=2 "$app"

zip="$out/Halyard-${version}-macos-universal.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"

echo "==> done"
lipo -info "$app/Contents/MacOS/halyard"
du -sh "$app" "$zip"
echo "$zip"
