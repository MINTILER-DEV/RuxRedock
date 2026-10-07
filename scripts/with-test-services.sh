#!/usr/bin/env bash
# Runs a command with disposable PostgreSQL and six-node Redis Cluster services.
set -euo pipefail
PG_BIN=${PG_BIN:-$(pg_config --bindir)}
REDIS_SERVER=${REDIS_SERVER:-redis-server}
if [[ ! -x "$PG_BIN/initdb" ]]; then echo 'Set PG_BIN to a PostgreSQL server bin directory.' >&2; exit 1; fi
command -v "$REDIS_SERVER" >/dev/null
test_workdir=$(mktemp -d "${TMPDIR:-/tmp}/ruxredock-services.XXXXXX")
redis_pids=()
cleanup() {
  "$PG_BIN/pg_ctl" -D "$test_workdir/postgres" -m immediate stop >/dev/null 2>&1 || true
  for pid in "${redis_pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${redis_pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  rm -rf "$test_workdir"
}
trap cleanup EXIT
read -r pg_port redis_port < <(python3 -c 'import socket, random
with socket.socket() as s:
 s.bind(("127.0.0.1",0)); pg=s.getsockname()[1]
while True:
 base=random.randrange(19000,25000); sockets=[]
 try:
  for p in [base+i for i in range(6)]+[base+10000+i for i in range(6)]:
   s=socket.socket(); sockets.append(s); s.bind(("127.0.0.1",p))
  print(pg,base); break
 except OSError: pass
 finally:
  for s in sockets:s.close()')
"$PG_BIN/initdb" -D "$test_workdir/postgres" -U ruxredock --auth=trust --no-locale --encoding=UTF8 >"$test_workdir/init.log"
"$PG_BIN/pg_ctl" -D "$test_workdir/postgres" -l "$test_workdir/postgres.log" -o "-h 127.0.0.1 -p $pg_port -k $test_workdir" start
createdb -h 127.0.0.1 -p "$pg_port" -U ruxredock ruxredock_tests
redis_nodes=()
for i in 0 1 2 3 4 5; do
  port=$((redis_port+i)); mkdir -p "$test_workdir/redis-$port"
  "$REDIS_SERVER" --port "$port" --bind 127.0.0.1 --protected-mode yes --cluster-enabled yes --cluster-config-file nodes.conf --cluster-node-timeout 2000 --appendonly no --save '' --dir "$test_workdir/redis-$port" >"$test_workdir/redis-$port/server.log" 2>&1 &
  redis_pids+=("$!"); redis_nodes+=("127.0.0.1:$port")
done
for attempt in {1..50}; do if redis-cli -p "$redis_port" ping >/dev/null 2>&1; then break; fi; sleep .1; done
redis-cli --cluster create "${redis_nodes[@]}" --cluster-replicas 1 --cluster-yes >"$test_workdir/cluster.log"
for attempt in {1..100}; do if redis-cli -p "$redis_port" cluster info | rg -q 'cluster_state:ok'; then break; fi; sleep .1; done
export TEST_DATABASE_URL="postgres://ruxredock@127.0.0.1:$pg_port/ruxredock_tests"
export TEST_REDIS_URLS="redis://127.0.0.1:$redis_port"
export TEST_REDIS_CLUSTER=true
echo 'Temporary PostgreSQL and six-node Redis Cluster are ready.'
"$@"
