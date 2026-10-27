#!/bin/bash
# Inspect the real app/helper payload without recording or opening a user profile.
set -euo pipefail

kind="${1:?usage: inspect-package.sh <app|core> <bundle> <architecture> [version]}"
bundle="${2:?a bundle path is required}"
architecture="${3:?an architecture is required}"
expected_version="${4:-}"
plist_value() {
    /usr/libexec/PlistBuddy -c "Print :$2" "$1/Contents/Info.plist"
}
require_identity() {
    /usr/bin/plutil -lint "$1/Contents/Info.plist"
    if [[ "$(plist_value "$1" CFBundleIdentifier)" != "$2" ]]; then
        echo "error: wrong bundle identity in $1" >&2
        exit 1
    fi
}

case "$kind" in
    app)
        require_identity "$bundle" com.aktanazat.sona.mac
        if [[ "$(plist_value "$bundle" CFBundleURLTypes:0:CFBundleURLSchemes:0)" != sona ]]; then
            echo "error: native app does not register sona links" >&2
            exit 1
        fi
        helper="$bundle/Contents/Helpers/SonaCore.app"
        camera="$bundle/Contents/Library/SystemExtensions/com.aktanazat.sona.mac.camera.systemextension"
        require_identity "$camera" com.aktanazat.sona.mac.camera
        for application in "$bundle" "$camera"; do
            executable="$application/Contents/MacOS/$(plist_value "$application" CFBundleExecutable)"
            [[ -x "$executable" ]] || { echo "error: missing app executable: $executable" >&2; exit 1; }
            /usr/bin/lipo -verify_arch "$architecture" "$executable"
        done
        ;;
    core) helper="$bundle" ;;
    *) echo "error: inspect either an app or a core bundle" >&2; exit 1 ;;
esac

require_identity "$helper" com.aktanazat.sona.core
helper_version="$(plist_value "$helper" CFBundleShortVersionString)"
if [[ -n "$expected_version" && "$helper_version" != "$expected_version" ]]; then
    echo "error: helper version $helper_version does not match release $expected_version" >&2
    exit 1
fi
if [[ "$kind" == app ]]; then
    for key in CFBundleShortVersionString CFBundleVersion LSMinimumSystemVersion; do
        if [[ "$(plist_value "$bundle" "$key")" != "$(plist_value "$helper" "$key")" ]]; then
            echo "error: app and core disagree on $key" >&2
            exit 1
        fi
    done
fi

macos="$helper/Contents/MacOS"
for name in sona-core sona-cleanup sona-agent-hook; do
    [[ -x "$macos/$name" ]] || { echo "error: missing packaged $name" >&2; exit 1; }
    /usr/bin/lipo -verify_arch "$architecture" "$macos/$name"
done
resources="$helper/Contents/Resources"
for resource in \
    _up_/LICENSE \
    _up_/NOTICE \
    resources/models/silero_vad_v4.onnx \
    resources/models/ten-vad.onnx \
    resources/models/history-semantic-recall.json \
    resources/models/meeting-diarization.json \
    resources/models/meeting-diarization-sortformer.json \
    resources/agent-bridge/sona-omp-agent-bridge.mjs \
    resources/ledger/ledger-template.html \
    resources/prompts/notes.txt \
    resources/cleanup-runtime/llama-LICENSE \
    resources/cleanup-runtime/json-LICENSE \
    resources/cleanup-runtime/model-LICENSE; do
    [[ -s "$resources/$resource" ]] || { echo "error: missing packaged resource: $resource" >&2; exit 1; }
done

core_version="$("$macos/sona-core" --version)"
if [[ "$core_version" != "sona $helper_version ("* ]]; then
    echo "error: packaged engine version does not match its bundle: $core_version" >&2
    exit 1
fi
printf '%s\n' "$core_version"
cleanup_version="$("$macos/sona-cleanup" --version)"
[[ "$cleanup_version" == "sona-cleanup "* ]] || { echo "error: packaged cleanup helper did not identify itself" >&2; exit 1; }
printf '%s\n' "$cleanup_version"

audit="$(mktemp -d "${TMPDIR:-/tmp}/sona-package.XXXXXX")"
trap 'rm -rf "$audit"' EXIT
hook_status=0
"$macos/sona-agent-hook" < /dev/null > "$audit/stdout" 2> "$audit/stderr" || hook_status=$?
if [[ "$hook_status" -ne 64 || -s "$audit/stdout" ]] || \
    ! grep -q '^usage: sona-agent-hook ' "$audit/stderr"; then
    echo "error: packaged agent hook did not return its usage refusal (exit $hook_status)" >&2
    exit 1
fi
printf '%s\n' "native $kind payload inspection passed; signing and notarization are separate checks"
