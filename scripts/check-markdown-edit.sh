#!/bin/bash
set -euo pipefail

if [[ ! -f macos/Tests/MarkdownEditTests.swift ]]; then
    printf '%s\n' 'Run this check from the Sona repository root.' >&2
    exit 1
fi

frameworks="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/Library/Frameworks"
swift_modules="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/usr/lib"
work="$(mktemp -d /private/var/tmp/sona-markdown-edit.XXXXXX)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/MarkdownEdit.xctest/Contents/MacOS"
xcrun swiftc -emit-library -I "$swift_modules" -L "$swift_modules" -F "$frameworks" \
    -Xlinker -rpath -Xlinker "$frameworks" -framework XCTest \
    -module-name MarkdownEdit macos/Sona/MarkdownEdit.swift macos/Tests/MarkdownEditTests.swift \
    -o "$work/MarkdownEdit.xctest/Contents/MacOS/MarkdownEdit"
xcrun xctest "$work/MarkdownEdit.xctest"
