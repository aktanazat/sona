#!/bin/bash
set -euo pipefail

if [[ ! -f macos/Tests/OnboardingTests.swift ]]; then
    printf '%s\n' 'Run this check from the Sona repository root.' >&2
    exit 1
fi

frameworks="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/Library/Frameworks"
swift_modules="$(xcode-select -p)/Platforms/MacOSX.platform/Developer/usr/lib"
work="$(mktemp -d /private/var/tmp/sona-onboarding.XXXXXX)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/Onboarding.xctest/Contents/MacOS"
xcrun swiftc -emit-library -I "$swift_modules" -L "$swift_modules" -F "$frameworks" \
    -Xlinker -rpath -Xlinker "$frameworks" -framework XCTest \
    -module-name Onboarding macos/Sona/Core.swift macos/Sona/CoreTypes.swift \
    macos/Sona/SettingsTypes.swift macos/Sona/PermissionsTypes.swift \
    macos/Sona/PermissionsStore.swift macos/Sona/OnboardingStore.swift \
    macos/Tests/OnboardingTests.swift -framework AppKit \
    -framework ApplicationServices -framework AVFoundation \
    -o "$work/Onboarding.xctest/Contents/MacOS/Onboarding"
xcrun xctest "$work/Onboarding.xctest"
