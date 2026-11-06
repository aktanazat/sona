#!/bin/bash
set -euo pipefail

if [[ ! -f macos/Tests/RecorderTests.swift ]]; then
    printf '%s\n' 'Run this check from the Sona repository root.' >&2
    exit 1
fi

frameworks="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/Library/Frameworks"
swift_modules="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/usr/lib"
work="$(mktemp -d /private/var/tmp/sona-recorder.XXXXXX)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/Recorder.xctest/Contents/MacOS"
xcrun swiftc -emit-library -I "$swift_modules" -L "$swift_modules" -F "$frameworks" \
    -Xlinker -rpath -Xlinker "$frameworks" -framework XCTest \
    -module-name Recorder macos/Sona/Core.swift macos/Sona/CoreTypes.swift \
    macos/Sona/RecorderTypes.swift macos/Tests/RecorderTests.swift \
    -o "$work/Recorder.xctest/Contents/MacOS/Recorder"
xcrun xctest "$work/Recorder.xctest"
