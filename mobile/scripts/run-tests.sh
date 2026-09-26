#!/bin/sh
# Build and run SonaTests in an iOS simulator.
#
# `xcodebuild ... build test` is the intended entry point, but on a Mac whose bundled
# iOS platform runtime is missing, xcodebuild enumerates no iOS Simulator destinations
# and refuses the test action while `build` still succeeds. This runs the same XCTest
# bundle in the same simulator through `simctl spawn xctest`.
#
# Usage: mobile/scripts/run-tests.sh [simulator-udid]
set -eu

project_dir=$(cd "$(dirname "$0")/.." && pwd)
device=${1:-$(xcrun simctl list devices available --json \
  | /usr/bin/python3 -c 'import json,sys
devices = json.load(sys.stdin)["devices"]
for runtime, entries in devices.items():
    for entry in entries:
        if entry["name"] == "iPhone 16":
            print(entry["udid"])
            raise SystemExit')}

build_dir=${TMPDIR:-/tmp}/sona-mobile-tests
xcodebuild -project "$project_dir/Sona.xcodeproj" -target SonaTests \
  -sdk iphonesimulator -arch arm64 -configuration Debug \
  CONFIGURATION_BUILD_DIR="$build_dir" build

xcrun simctl bootstatus "$device" -b >/dev/null 2>&1 || xcrun simctl boot "$device" || true

runtime_root=$(xcrun simctl getenv "$device" IPHONE_SIMULATOR_ROOT)
platform=$(xcode-select -p)/Platforms/iPhoneSimulator.platform/Developer

SIMCTL_CHILD_DYLD_FRAMEWORK_PATH="$platform/Library/Frameworks" \
SIMCTL_CHILD_DYLD_LIBRARY_PATH="$platform/usr/lib" \
  xcrun simctl spawn "$device" "$runtime_root/Developer/usr/bin/xctest" \
    "$build_dir/SonaTests.xctest"
