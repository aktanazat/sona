#!/bin/bash
# Run from the repository root. Uses synthetic video only: no camera, microphone,
# meeting app, Accessibility permission, installation, or system approval.
set -euo pipefail

if [[ ! -f src-tauri/swift/call_chat.swift ]]; then
    printf '%s\n' 'Run this check from the Sona repository root.' >&2
    exit 1
fi

frameworks="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/Library/Frameworks"
swift_modules="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/usr/lib"
work="$(mktemp -d /private/var/tmp/sona-call-notice.XXXXXX)"
trap 'rm -rf "$work"' EXIT

flags=(-emit-library -I "$swift_modules" -L "$swift_modules" -F "$frameworks" -Xlinker -rpath -Xlinker "$frameworks" -framework XCTest)
mkdir -p "$work/CallChat.xctest/Contents/MacOS" "$work/CameraWatermark.xctest/Contents/MacOS"
xcrun swiftc "${flags[@]}" -module-name CallChat \
    src-tauri/swift/call_chat.swift src-tauri/swift/call_chat_tests.swift \
    -framework AppKit -framework ApplicationServices -o "$work/CallChat.xctest/Contents/MacOS/CallChat"
xcrun xctest "$work/CallChat.xctest"

xcrun swiftc "${flags[@]}" -module-name CameraWatermark \
    macos/CameraShared/CameraWatermarkState.swift \
    macos/SonaCamera/CameraFrameRenderer.swift macos/Tests/CameraWatermarkTests.swift \
    -framework CoreImage -framework CoreMedia -framework CoreVideo -framework CoreText \
    -o "$work/CameraWatermark.xctest/Contents/MacOS/CameraWatermark"
xcrun xctest "$work/CameraWatermark.xctest"
