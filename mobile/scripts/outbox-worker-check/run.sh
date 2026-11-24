#!/bin/sh
# The phone's real UploadQueue against a local copy of the real companion Worker
# (app-audit SONA-C3 and SONA-C4). Everything runs on this Mac on 127.0.0.1: the Worker
# under `wrangler dev --local` with its own state folder, never a deployed one.
#
# Usage: mobile/scripts/outbox-worker-check/run.sh [scratch-dir]
# Exits 0 when both checks pass.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
scratch=${1:-${TMPDIR:-/private/var/tmp}/outbox-worker-check}
worker_port=8799
proxy_port=8798
secret=local-outbox-check
vault_id=vault_outbox_check_$(date +%s)

rm -rf "$scratch"
mkdir -p "$scratch"
rsync -a --exclude node_modules "$repo/cloudflare/sona-companion/" "$scratch/worker/"
cd "$scratch/worker"
# Local only: no custom domain route, metrics off.
/usr/bin/python3 - wrangler.jsonc <<'EOF'
import re, sys
path = sys.argv[1]
text = open(path).read()
text = re.sub(r'\n\s*"routes": \[[^\]]*\],', "", text)
open(path, "w").write(text)
EOF
export WRANGLER_SEND_METRICS=false
bun install --frozen-lockfile >"$scratch/install.log" 2>&1
printf 'BOOTSTRAP_SECRET=%s\n' "$secret" >.dev.vars
./node_modules/.bin/wrangler d1 migrations apply sona-companion --local --persist-to "$scratch/state" \
  >"$scratch/migrate.log" 2>&1

for port in "$worker_port" "$proxy_port"; do
  if lsof -t -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "port $port is in use; stop what holds it first" >&2
    exit 1
  fi
done
cleanup() {
  [ -n "${proxy_pid:-}" ] && kill "$proxy_pid" 2>/dev/null || true
  [ -n "${worker_pid:-}" ] && kill "$worker_pid" 2>/dev/null || true
  # wrangler's workerd children run from this scratch copy only.
  pkill -f "$scratch/worker/node_modules" 2>/dev/null || true
  for port in "$worker_port" "$proxy_port"; do
    lsof -t -iTCP:"$port" -sTCP:LISTEN 2>/dev/null | xargs kill 2>/dev/null || true
  done
}
trap cleanup EXIT INT TERM

./node_modules/.bin/wrangler dev --local --ip 127.0.0.1 --port "$worker_port" \
  --persist-to "$scratch/state" >"$scratch/worker.log" 2>&1 &
worker_pid=$!
(cd "$repo" && exec bun "$here/aid.ts" proxy "$proxy_port" "http://127.0.0.1:$worker_port") \
  >"$scratch/proxy.log" 2>&1 &
proxy_pid=$!
attempt=0
until curl -s -o /dev/null "http://127.0.0.1:$worker_port/healthz" \
    && curl -s -o /dev/null "http://127.0.0.1:$proxy_port/__check/last-upload"; do
  attempt=$((attempt + 1))
  [ "$attempt" -le 120 ] || { tail -30 "$scratch/worker.log" "$scratch/proxy.log"; exit 1; }
  sleep 1
done

shared=$repo/mobile/Shared
swiftc -O -parse-as-library -o "$scratch/check" "$here/main.swift" \
  "$shared/UploadQueue.swift" "$shared/CompanionClient.swift" "$shared/DeviceRecordingObject.swift" \
  "$shared/SonaCrypto.swift" "$shared/ThoughtObject.swift" "$shared/VaultKeychain.swift" \
  "$shared/AgentAsk.swift" "$shared/CanonicalRecord.swift" "$shared/PCMResampler.swift" "$shared/Pairing.swift"

"$scratch/check" mint "$scratch/identity.json"
(cd "$repo" && bun "$here/aid.ts" bootstrap "$scratch/identity.json" "$vault_id" \
  "http://127.0.0.1:$worker_port" "$secret")

# The state maintenance leaves a purged session in (src/maintenance.ts).
purge="cd '$scratch/worker' && ./node_modules/.bin/wrangler d1 execute sona-companion --local \
--persist-to '$scratch/state' --command \"UPDATE upload_sessions SET state = 'purged', \
cleanup_accounted = 1, expires_at = 0 WHERE upload_id = '\$1'\" >>'$scratch/purge.log' 2>&1"
status=0
"$scratch/check" run "$scratch/identity.json" "$vault_id" "http://127.0.0.1:$proxy_port" "$purge" || status=$?
cleanup
exit "$status"
