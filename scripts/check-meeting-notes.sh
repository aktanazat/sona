#!/bin/bash
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"
frameworks="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/Library/Frameworks"
swift_modules="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/usr/lib"
work="$(mktemp -d /private/var/tmp/sona-meeting-notes.XXXXXX)"
trap 'rm -rf "$work"' EXIT
build="${1:-$work/build}"

xcodebuild -quiet -project macos/Sona.xcodeproj -scheme Sona -configuration Debug \
    -destination generic/platform=macOS -derivedDataPath "$build" ARCHS="$(uname -m)" ONLY_ACTIVE_ARCH=YES \
    SONA_CORE_TARGET_DIR="$work/no-core" CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO build
products="$build/Build/Products/Debug"
app_bin="$products/Sona.app/Contents/MacOS"
mkdir -p "$work/MeetingNotes.xctest/Contents/MacOS"
xcrun swiftc -swift-version 6 -emit-library -I "$products" \
    -I "$swift_modules" -L "$swift_modules" -F "$frameworks" \
    -Xlinker -rpath -Xlinker "$frameworks" -framework XCTest \
    -Xlinker -rpath -Xlinker "$app_bin" "$app_bin/Sona.debug.dylib" \
    -module-name MeetingNotes macos/Tests/MeetingNotesTests.swift \
    -o "$work/MeetingNotes.xctest/Contents/MacOS/MeetingNotes"
xcrun xctest "$work/MeetingNotes.xctest"
