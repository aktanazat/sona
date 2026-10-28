#!/bin/bash
# Xcode owns the outer app signature. This phase prepares and signs its private core.
set -euo pipefail

root="${SRCROOT:?}/.."
target="${SONA_CORE_TARGET_DIR:-$root/src-tauri/target}"
helper="${TARGET_BUILD_DIR:?}/${CONTENTS_FOLDER_PATH:?}/Helpers/SonaCore.app"
case "${CONFIGURATION:?}" in
    Debug) profile=debug ;;
    Release) profile=release ;;
    *) echo "error: unsupported core configuration: $CONFIGURATION" >&2; exit 1 ;;
esac

# Never let a previous build's helper survive a missing or incomplete new build.
rm -rf "$helper"
core="$target/$profile/sona"
if [[ ! -x "$core" ]]; then
    if [[ "$CONFIGURATION" == Debug ]]; then
        echo "warning: no debug core at $core; set SONA_CORE when running this development app"
        exit 0
    fi
    echo "error: Release requires $core; prepare both helpers and build the release core first" >&2
    exit 1
fi

case "${ARCHS:?}" in
    arm64) triple=aarch64-apple-darwin ;;
    x86_64) triple=x86_64-apple-darwin ;;
    *) echo "error: build one Mac architecture at a time with ARCHS=arm64 or ARCHS=x86_64" >&2; exit 1 ;;
esac
hook="$target/$triple/$profile/sona-agent-hook"
cleanup="$target/cleanup-runtime/$triple/stage/sona-cleanup"
for executable in "$core" "$hook" "$cleanup"; do
    if [[ ! -x "$executable" ]]; then
        echo "error: required core executable is missing: $executable" >&2
        exit 1
    fi
    /usr/bin/lipo -verify_arch "$ARCHS" "$executable"
done

mkdir -p "$helper/Contents/MacOS" "$helper/Contents/Resources/_up_"
cp "$core" "$helper/Contents/MacOS/sona-core"
cp "$hook" "$helper/Contents/MacOS/sona-agent-hook"
cp "$cleanup" "$helper/Contents/MacOS/sona-cleanup"
chmod 755 "$helper/Contents/MacOS/sona-core" \
    "$helper/Contents/MacOS/sona-agent-hook" "$helper/Contents/MacOS/sona-cleanup"
/usr/bin/ditto "$root/src-tauri/resources" "$helper/Contents/Resources/resources"
cp "$root/LICENSE" "$root/NOTICE" "$helper/Contents/Resources/_up_/"

cat > "$helper/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>sona-core</string>
    <key>CFBundleIdentifier</key>
    <string>com.aktanazat.sona.core</string>
    <key>CFBundleName</key>
    <string>Sona Core</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${MARKETING_VERSION:?}</string>
    <key>CFBundleVersion</key>
    <string>${CURRENT_PROJECT_VERSION:?}</string>
    <key>LSMinimumSystemVersion</key>
    <string>${MACOSX_DEPLOYMENT_TARGET:?}</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Sona hears you only while a recording is on.</string>
    <key>NSCalendarsFullAccessUsageDescription</key>
    <string>Read upcoming events and create calendar events you approve.</string>
    <key>NSCalendarsUsageDescription</key>
    <string>Read upcoming events and create calendar events you approve.</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>Read recent email only when you enable it, and open Mail drafts you approve. Sona never sends mail.</string>
</dict>
</plist>
PLIST

/bin/bash "$SRCROOT/scripts/inspect-package.sh" core "$helper" "$ARCHS"
if [[ "${CODE_SIGNING_ALLOWED:-YES}" != NO ]]; then
    identity="${EXPANDED_CODE_SIGN_IDENTITY:?a signing identity is required when code signing is enabled}"
    /usr/bin/codesign --force --options runtime --sign "$identity" \
        "$helper/Contents/MacOS/sona-agent-hook"
    /usr/bin/codesign --force --options runtime --sign "$identity" \
        "$helper/Contents/MacOS/sona-cleanup"
    /usr/bin/codesign --force --options runtime --sign "$identity" \
        --entitlements "$root/src-tauri/Entitlements.plist" "$helper"
    /usr/bin/codesign --verify --deep --strict "$helper"
fi
