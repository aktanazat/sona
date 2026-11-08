#!/bin/bash
set -euo pipefail

if [[ ! -f macos/Tests/ThemeTests.swift ]]; then
    printf '%s\n' 'Run this check from the Sona repository root.' >&2
    exit 1
fi

frameworks="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/Library/Frameworks"
swift_modules="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/usr/lib"
work="$(mktemp -d /private/var/tmp/sona-theme.XXXXXX)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/ThemeCheck.xctest/Contents/MacOS"
xcrun swiftc -emit-library -I "$swift_modules" -L "$swift_modules" -F "$frameworks" \
    -Xlinker -rpath -Xlinker "$frameworks" -framework XCTest \
    -module-name ThemeCheck macos/Sona/Theme.swift macos/Tests/ThemeTests.swift \
    -o "$work/ThemeCheck.xctest/Contents/MacOS/ThemeCheck"
xcrun xctest "$work/ThemeCheck.xctest"
