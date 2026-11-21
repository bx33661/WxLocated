#!/bin/zsh
# Release build: WxLocated.app (universal), then a zip, an installer package
# and a disk image, with SHA-256 sums. Signs and notarizes when the
# identities are given; otherwise the app is signed ad hoc.
#
#   VERSION=1.1.0 scripts/build-release.sh
#   DEVELOPER_ID_APPLICATION="Developer ID Application: …" \
#   DEVELOPER_ID_INSTALLER="Developer ID Installer: …" \
#   NOTARY_KEYCHAIN_PROFILE=wxlocated scripts/build-release.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
VERSION="${VERSION:-$CARGO_VERSION}"
STAMP="$(date +%Y%m%d-%H%M%S)"
OUTPUT_ROOT="${OUTPUT_DIR:-$ROOT/dist/WxLocated-$VERSION-$STAMP}"
STAGE="$OUTPUT_ROOT/stage"
APP="$STAGE/WxLocated.app"
DMG="$OUTPUT_ROOT/WxLocated-$VERSION-macos-universal.dmg"
PKG="$OUTPUT_ROOT/WxLocated-$VERSION-macos-universal.pkg"
APP_ARCHIVE="$OUTPUT_ROOT/WxLocated-$VERSION-macos-universal.zip"

if [[ "$VERSION" != "$CARGO_VERSION" ]]; then
  echo "Requested version $VERSION does not match the workspace version $CARGO_VERSION (Cargo.toml)" >&2
  exit 2
fi

mkdir -p "$STAGE"
OUTPUT_DIR="$STAGE" SIGN_IDENTITY="${DEVELOPER_ID_APPLICATION:--}" "$ROOT/scripts/bundle-macos.sh"

for executable in "$APP/Contents/MacOS/WxLocated" "$APP/Contents/Helpers/wxlocated"; do
  architectures="$(lipo -archs "$executable")"
  if [[ "$architectures" != "x86_64 arm64" && "$architectures" != "arm64 x86_64" ]]; then
    echo "Expected arm64 and x86_64 in $executable, found: $architectures" >&2
    exit 3
  fi
done
for arch in arm64 x86_64; do
  output="$(/usr/bin/arch -"$arch" "$APP/Contents/Helpers/wxlocated" --version)"
  grep -Fq "\"$VERSION\"" <<< "$output" || {
    echo "Bundled $arch CLI does not report version $VERSION" >&2
    exit 3
  }
done
codesign --verify --deep --strict --verbose=2 "$APP"

ditto -c -k --sequesterRsrc --keepParent "$APP" "$APP_ARCHIVE"

COMPONENT_PLIST="$OUTPUT_ROOT/component.plist"
pkgbuild --analyze --root "$STAGE" "$COMPONENT_PLIST" >/dev/null
plutil -replace 0.BundleIsRelocatable -bool false "$COMPONENT_PLIST"
PKG_ARGUMENTS=(--root "$STAGE" --component-plist "$COMPONENT_PLIST" --install-location /Applications --identifier com.wxlocated.WxLocated --version "$VERSION")
if [[ -n "${DEVELOPER_ID_INSTALLER:-}" ]]; then
  PKG_ARGUMENTS+=(--sign "$DEVELOPER_ID_INSTALLER")
fi
pkgbuild "${PKG_ARGUMENTS[@]}" "$PKG"
rm -f "$COMPONENT_PLIST"

ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "WxLocated $VERSION" -srcfolder "$STAGE" -format UDZO -ov "$DMG" >/dev/null
rm "$STAGE/Applications"

if [[ -n "${DEVELOPER_ID_APPLICATION:-}" ]]; then
  codesign --force --timestamp --sign "$DEVELOPER_ID_APPLICATION" "$DMG"
  codesign --verify --verbose=2 "$DMG"
fi

if [[ -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
  if [[ -z "${DEVELOPER_ID_APPLICATION:-}" || -z "${DEVELOPER_ID_INSTALLER:-}" ]]; then
    echo "NOTARY_KEYCHAIN_PROFILE requires DEVELOPER_ID_APPLICATION and DEVELOPER_ID_INSTALLER" >&2
    exit 2
  fi
  for artifact in "$DMG" "$PKG"; do
    xcrun notarytool submit "$artifact" --keychain-profile "$NOTARY_KEYCHAIN_PROFILE" --wait
    xcrun stapler staple "$artifact"
    xcrun stapler validate "$artifact"
  done
fi

(
  cd "$OUTPUT_ROOT"
  shasum -a 256 "$(basename "$DMG")" "$(basename "$PKG")" "$(basename "$APP_ARCHIVE")" > SHA256SUMS.txt
)

echo "Release directory: $OUTPUT_ROOT"
echo "Application: $APP"
echo "Disk image: $DMG"
echo "Installer package: $PKG"
echo "Application archive: $APP_ARCHIVE"
echo "Checksums: $OUTPUT_ROOT/SHA256SUMS.txt"
