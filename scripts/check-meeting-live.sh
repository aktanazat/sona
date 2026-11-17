#!/bin/bash
# Decodes the meeting status payloads the core sends, against the app's own
# Swift types. Builds every app source, so it takes a few minutes.
set -euo pipefail

if [[ ! -f macos/Tests/MeetingLiveTests.swift ]]; then
    printf '%s\n' 'Run this check from the Sona repository root.' >&2
    exit 1
fi

frameworks="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/Library/Frameworks"
swift_modules="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/usr/lib"
work="$(mktemp -d /private/var/tmp/sona-meeting-live.XXXXXX)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/MeetingLive.xctest/Contents/MacOS"
xcrun swiftc -emit-library -parse-as-library -enable-bare-slash-regex \
    -I "$swift_modules" -L "$swift_modules" -F "$frameworks" \
    -Xlinker -rpath -Xlinker "$frameworks" -framework XCTest \
    -module-name MeetingLive macos/Sona/*.swift macos/CameraShared/*.swift \
    macos/Tests/MeetingLiveTests.swift \
    -o "$work/MeetingLive.xctest/Contents/MacOS/MeetingLive"
xcrun xctest "$work/MeetingLive.xctest"
