#!/bin/bash
# A Mac launches a device-bound build only when the embedded profile lists
# its provisioning UDID; otherwise AMFI refuses it with -413.
set -euo pipefail

[[ "${CODE_SIGNING_ALLOWED:-YES}" == YES ]] || exit 0
required="${SONA_PROVISIONED_DEVICES:-}"
[[ -n "$required" ]] || exit 0

embedded="${CODESIGNING_FOLDER_PATH:?}/Contents/embedded.provisionprofile"
if [[ ! -f "$embedded" ]]; then
    echo "error: $embedded is missing; Release must embed the profile that lists $required" >&2
    exit 1
fi

decoded="$(mktemp)"
trap 'rm -f "$decoded"' EXIT
security cms -D -i "$embedded" > "$decoded"
uuid="$(plutil -extract UUID raw -o - "$decoded")"
devices="$(plutil -extract ProvisionedDevices xml1 -o - "$decoded" 2>/dev/null || true)"

missing=0
for udid in $required; do
    if ! grep -q "<string>$udid</string>" <<<"$devices"; then
        echo "error: embedded profile $uuid does not list device $udid" >&2
        missing=1
    fi
done
[[ "$missing" == 0 ]] || exit 1
echo "embedded profile $uuid lists $required"
