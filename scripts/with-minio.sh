#!/usr/bin/env bash
# Run a command against an ephemeral four-node, eight-drive MinIO deployment.
set -euo pipefail
MINIO_BIN=${MINIO_BIN:-minio}
command -v "$MINIO_BIN" >/dev/null
test_workdir=$(mktemp -d "${TMPDIR:-/tmp}/ruxredock-minio.XXXXXX")
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  rm -rf "$test_workdir"
}
trap cleanup EXIT
base=$(python3 - <<'PY'
import random, socket
while True:
 base=random.randrange(26000,29000); sockets=[]
 try:
  for port in range(base,base+8):
   s=socket.socket();sockets.append(s);s.bind(('127.0.0.1',port))
  print(base);break
 except OSError: pass
 finally:
  for s in sockets:s.close()
PY
)
# Test directories share a filesystem; real deployments use separate mounted drives.
export MINIO_CI_CD=1
export MINIO_ROOT_USER=ruxredock-test
export MINIO_ROOT_PASSWORD=ruxredock-ephemeral-test-secret
endpoints=()
for node in 0 1 2 3; do
  for drive in 1 2; do
    mkdir -p "$test_workdir/node$node/drive$drive"
    endpoints+=("http://127.0.0.1:$((base+node))$test_workdir/node$node/drive$drive")
  done
done
for node in 0 1 2 3; do
  "$MINIO_BIN" server --address "127.0.0.1:$((base+node))" --console-address "127.0.0.1:$((base+node+4))" "${endpoints[@]}" >"$test_workdir/node$node.log" 2>&1 &
  pids+=("$!")
done
export S3_ENDPOINT="http://127.0.0.1:$base"
ready=false
for attempt in {1..120}; do
  if curl --max-time 1 --silent --fail "$S3_ENDPOINT/minio/health/cluster" >/dev/null; then ready=true;break;fi
  sleep .25
done
if [[ "$ready" != true ]]; then cat "$test_workdir"/node*.log >&2;exit 1;fi
export S3_BUCKET=ruxredock-tests AWS_REGION=us-east-1
export AWS_ACCESS_KEY_ID="$MINIO_ROOT_USER" AWS_SECRET_ACCESS_KEY="$MINIO_ROOT_PASSWORD"
curl --max-time 5 --retry 20 --retry-delay 1 --retry-all-errors --silent --show-error --fail --aws-sigv4 'aws:amz:us-east-1:s3' --user "$AWS_ACCESS_KEY_ID:$AWS_SECRET_ACCESS_KEY" -X PUT "$S3_ENDPOINT/$S3_BUCKET"
echo 'Temporary four-node MinIO with eight drives is ready.'
"$@"
