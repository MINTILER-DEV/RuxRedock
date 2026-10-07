#!/usr/bin/env bash
# Use with with-test-services.sh; optionally nest inside with-minio.sh.
set -euo pipefail
export DATABASE_URL=${TEST_DATABASE_URL:?Run inside scripts/with-test-services.sh}
export REDIS_URLS=${TEST_REDIS_URLS:?} REDIS_CLUSTER=${TEST_REDIS_CLUSTER:-true}
export CACHE_NAMESPACE="ruxredock:browser:$$" DEDUP_SCOPE=tenant MIN_RESPONSE_MS=10
if [[ -n "${S3_ENDPOINT:-}" ]]; then export STORAGE_BACKEND=s3;fi
browser_workdir=$(mktemp -d "${TMPDIR:-/tmp}/ruxredock-browser.XXXXXX")
server_pid=''
cleanup() {
  if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || true;wait "$server_pid" 2>/dev/null || true;fi
  rm -rf "$browser_workdir"
}
trap cleanup EXIT
export STORAGE_DIR="$browser_workdir/objects"
port=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')
export BIND_ADDR="127.0.0.1:$port" BROWSER_BASE_URL="http://127.0.0.1:$port"
cargo build --locked -p RuxRedock
BROWSER_API_TOKEN=$(target/debug/RuxRedock create-user 'Server verification' | python3 -c 'import json,sys;print(json.load(sys.stdin)["token"])')
export BROWSER_API_TOKEN
target/debug/RuxRedock serve >"$browser_workdir/server.log" 2>&1 &
server_pid=$!
for attempt in {1..100};do
  if curl --silent --fail "$BROWSER_BASE_URL/health" >/dev/null;then break;fi
  if ! kill -0 "$server_pid" 2>/dev/null;then cat "$browser_workdir/server.log" >&2;exit 1;fi
  sleep .1
done
npm test --prefix frontend -- --grep 'private server'
