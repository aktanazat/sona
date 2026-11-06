#!/bin/bash
set -euo pipefail

if [[ ! -f macos/Tests/LibraryTests.swift ]]; then
    printf '%s\n' 'Run this check from the Sona repository root.' >&2
    exit 1
fi

frameworks="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/Library/Frameworks"
swift_modules="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/usr/lib"
work="$(mktemp -d /private/var/tmp/sona-library.XXXXXX)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/Library.xctest/Contents/MacOS"
xcrun swiftc -emit-library -I "$swift_modules" -L "$swift_modules" -F "$frameworks" \
    -Xlinker -rpath -Xlinker "$frameworks" -framework XCTest \
    -module-name Library macos/Sona/CoreTypes.swift macos/Sona/Models.swift \
    macos/Sona/PrivacyTypes.swift macos/Sona/LibraryTypes.swift \
    macos/Tests/LibraryTests.swift \
    -o "$work/Library.xctest/Contents/MacOS/Library"
xcrun xctest "$work/Library.xctest"
