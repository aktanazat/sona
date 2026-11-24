#!/bin/bash
set -euo pipefail

if [[ ! -f macos/Tests/WorkViewTests.swift ]]; then
    printf '%s\n' 'Run this check from the Sona repository root.' >&2
    exit 1
fi

frameworks="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/Library/Frameworks"
swift_modules="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/usr/lib"
work="$(mktemp -d /private/var/tmp/sona-work-views.XXXXXX)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/WorkViews.xctest/Contents/MacOS"
xcrun swiftc -emit-library -I "$swift_modules" -L "$swift_modules" -F "$frameworks" \
    -Xlinker -rpath -Xlinker "$frameworks" -framework XCTest \
    -module-name WorkViews macos/Sona/WorkViewTypes.swift macos/Tests/WorkViewTests.swift \
    -o "$work/WorkViews.xctest/Contents/MacOS/WorkViews"
xcrun xctest "$work/WorkViews.xctest"
