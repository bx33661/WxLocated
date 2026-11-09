#!/bin/zsh
# Builds WxLocated.app from the Rust workspace: the GPUI app as the bundle
# executable and the `wxlocated` CLI in Contents/Helpers (Contents/MacOS
# already holds `WxLocated`, the same name on a case-insensitive volume).
# Both are universal binaries (Apple silicon and Intel), macOS 14 or later.
#
#   scripts/bundle-macos.sh                # ad-hoc signed, target/bundle/WxLocated.app
#   SIGN_IDENTITY="Developer ID Application: …" scripts/bundle-macos.sh
#   ARCHS=aarch64 scripts/bundle-macos.sh  # this Mac's architecture only, faster
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUTPUT_DIR:-$ROOT/target/bundle}"
APP="$OUT/WxLocated.app"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
ICONS="$ROOT/crates/wxlocated-app/assets/AppIcon.iconset"
ARCHS=(${=ARCHS:-aarch64 x86_64})
export MACOSX_DEPLOYMENT_TARGET=14.0

# The CLI is built on its own: building it together with the app would turn
# on dependency features (serde_json preserve_order) that only the app needs.
binaries=()
for arch in $ARCHS; do
  target="$arch-apple-darwin"
  cargo build --release --manifest-path "$ROOT/Cargo.toml" --target "$target" -p wxlocated-cli
  cargo build --release --manifest-path "$ROOT/Cargo.toml" --target "$target" -p wxlocated-app
  binaries+=("$ROOT/target/$target/release")
done

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/WxLocated" "${^binaries[@]}/wxlocated-app"
lipo -create -output "$APP/Contents/Helpers/wxlocated" "${^binaries[@]}/wxlocated"

ICONSET="$(mktemp -d)/AppIcon.iconset"
cp -R "$ICONS" "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>WxLocated</string>
  <key>CFBundleIdentifier</key><string>com.wxlocated.WxLocated</string>
  <key>CFBundleName</key><string>WxLocated</string>
  <key>CFBundleDisplayName</key><string>WxLocated</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleDevelopmentRegion</key><string>zh_CN</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>WxLocated</string>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>微信小程序包</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Owner</string>
      <key>LSItemContentTypes</key><array><string>com.wxlocated.wxapkg</string></array>
    </dict>
    <dict>
      <key>CFBundleTypeName</key><string>小程序包或 Agent 重建层目录</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key><array><string>public.folder</string></array>
    </dict>
  </array>
  <key>UTExportedTypeDeclarations</key>
  <array>
    <dict>
      <key>UTTypeIdentifier</key><string>com.wxlocated.wxapkg</string>
      <key>UTTypeDescription</key><string>微信小程序包</string>
      <key>UTTypeConformsTo</key><array><string>public.data</string></array>
      <key>UTTypeTagSpecification</key>
      <dict><key>public.filename-extension</key><array><string>wxapkg</string></array></dict>
    </dict>
  </array>
</dict>
</plist>
PLIST

IDENTITY="${SIGN_IDENTITY:--}"
TIMESTAMP=(--timestamp=none)
[[ "$IDENTITY" != "-" ]] && TIMESTAMP=(--timestamp)
codesign --force --options runtime "${TIMESTAMP[@]}" -s "$IDENTITY" "$APP/Contents/Helpers/wxlocated"
codesign --force --options runtime "${TIMESTAMP[@]}" -s "$IDENTITY" "$APP"
codesign --verify --strict "$APP"
echo "$APP"
