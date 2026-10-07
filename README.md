# RuxRedock

A file manager backed by client-side content-defined chunking, convergent encryption, PostgreSQL metadata, Redis indexing, and a content-addressed object store. Phases 1–3 of [the architecture plan](local/plan.md) are implemented: the Python proof of concept lives in `poc/`, the Rust API in `src/`, the Rust browser library in `wasm/`, and the React dashboard in `frontend/`.

The dashboard provides folder navigation, breadcrumbs, searchable and sortable file rows, grid view, starred/recent files, a details panel, version history, upload progress and cancellation, and authenticated downloads. It opens with a clearly labeled local demo using real encrypted files in IndexedDB. **Connect your server** switches to the live API. Demo files never leave the browser.

## Try the frontend

Requirements: Node.js 20+, Rust with the `wasm32-unknown-unknown` target, and `wasm-pack`.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack --locked
npm ci --prefix frontend
npm run build --prefix frontend
npm run dev --prefix frontend
```

Open http://127.0.0.1:5173. The development server proxies `/v1` and `/health` to the backend on port 8080. Build generates the Wasm bindings before bundling the worker; generated bindings and build output are ignored by Git. Rebuild with `npm run build:wasm --prefix frontend` after Rust client changes.

## Run the complete stack

Docker Compose builds the dashboard/API and a pinned MinIO source release, then starts PostgreSQL 16, six Redis nodes (three masters and three replicas), four MinIO nodes with eight persistent volumes, an S3 gateway, and bucket provisioning. The first build compiles Rust and MinIO and can take several minutes.

```bash
docker compose up --build -d
docker compose exec api RuxRedock create-user 'My workspace'
```

Open http://127.0.0.1:8080, click **Connect your server**, and paste the returned token. Leave the recovery key blank for a new workspace, then save the generated key. Use that same key when reconnecting to decrypt earlier uploads. Access tokens and recovery keys stay in frontend session memory; reloading requires reconnecting. The local demo alone stores its demo key locally.

Compose defaults are for local development. Set `POSTGRES_PASSWORD`, `MINIO_ROOT_USER`, and `MINIO_ROOT_PASSWORD` before creating the stack for another environment. The API is bound to localhost. Preserve the PostgreSQL and all MinIO volumes together when backing up or moving the service. Deleting a file releases its logical quota; physical objects are retained for shared references. Garbage collection is not implemented.

## Run the backend directly

Install PostgreSQL 15+ and Redis, create a database, and configure the environment using [.env.example](.env.example). The binary reads environment variables; it does not automatically load `.env`.

```bash
set -a
source .env
set +a
cargo run -- create-user 'My workspace'
cargo run -- serve
```

Run from the repository root so the server can serve `frontend/dist`. Migrations run automatically at startup and during account creation; `cargo run -- migrate` applies them independently. The default storage backend writes atomically to `.data/objects`. For MinIO or another S3 endpoint, set `STORAGE_BACKEND=s3`, `S3_ENDPOINT`, `S3_BUCKET`, and the AWS credentials/region shown in the example. Provision the bucket first. Object writes use conditional creation; existing objects and downloaded ciphertext are checked for hash and size integrity.

`REDIS_CLUSTER=true` enables cluster connections through comma-separated `REDIS_URLS` seed nodes. Redis is a positive cache, not the authority for file ownership or upload completion. Cache outages fall back to PostgreSQL; a stale positive result is repaired by the upload client's missing-block retry. Use a distinct `CACHE_NAMESPACE` per database/environment.

## Encryption and upload policy

A Web Worker performs BuzHash chunking, SHA-256, HKDF, and AES-256-GCM through Rust WebAssembly. Browser defaults are a 64-byte window, 16 KiB minimum, 64 KiB boundary target, and 256 KiB maximum. The object format matches the Python PoC. Scanning uses bounded input buffers; only requested blocks are encrypted again and uploaded with four concurrent requests. The manifest retains plaintext fingerprints and the whole-file hash and is sealed using an independent random recovery key before reaching the server. Downloads authenticate each block, verify plaintext fingerprints, and verify the complete file hash before saving.

The server sees filenames, hierarchy, sizes, encrypted object identifiers, and sealed manifest data. Convergent encryption exposes equality and permits guesses of predictable plaintext; it is not a substitute for randomized encryption where that threat matters. No secret key-derivation server or pepper is claimed here.

`DEDUP_SCOPE=tenant` is the default. A user can skip a block upload only after that account has supplied the actual ciphertext. Physical storage still deduplicates globally, but another user's possession does not make the missing-block API expose a cache hit. `DEDUP_SCOPE=global` explicitly enables cross-account upload skipping and reveals matching block presence; use it only for data where that exposure is acceptable.

All authenticated API responses have a configurable minimum delay (`MIN_RESPONSE_MS`, default 40). Upload completion additionally waits for a simulated baseline transfer duration derived from logical file size (`SIMULATED_BYTES_PER_SECOND`, default 20 MiB/s). A version remains pending throughout this delay, including through metadata polling, and the delay holds no database connection. These measures reduce obvious timing differences; they do not prove elimination of every network side channel.

## Limits

The API accepts at most 250,000 chunks, 1 MiB per plaintext chunk, 100 GiB per file, and a 128 MiB manifest request. Actual maximum file size also depends on chunk count. Quota counts every version's logical size, including pending uploads; cancellation refunds it. Downloads stream to disk in browsers supporting the File System Access API. Other browsers use a verified Blob fallback capped at 256 MiB. The dashboard's search/stars/recent views load workspace metadata; very large workspaces will need server-side search and virtualized listings.

## Verification

```bash
cargo test --workspace
python3 -m unittest discover -s poc -v
npm run check --prefix frontend
npx --prefix frontend playwright install chromium
npm test --prefix frontend
```

Install `poc/requirements.txt` for the Python tests. Browser tests exercise file operations, persistence, binary round trips, deduplication, responsive navigation, and Python/Wasm compatibility vectors.

The following helpers create temporary services and clean them up after the command. PostgreSQL must run as a non-root user; PostgreSQL server binaries, client tools, Redis server/client, Python, curl, and ripgrep must be installed. Set `PG_BIN` to the PostgreSQL server bin directory and `REDIS_SERVER` if it is outside PATH.

```bash
bash scripts/with-test-services.sh cargo test --test api -- --ignored
# Supply an installed MinIO binary via MINIO_BIN if needed:
bash scripts/with-minio.sh cargo test --test s3 -- --ignored
# Test the production dashboard against the entire live stack:
npm run build --prefix frontend
bash scripts/with-minio.sh bash scripts/with-test-services.sh bash scripts/verify-browser-server.sh
```

The MinIO helper runs four processes and eight test directories on one filesystem using MinIO's CI mode. The Compose deployment uses separate persistent volumes. API integration tests cover concurrent quota accounting, ownership, stale/offline caches, global reuse, tenant proof of possession, manifest size poisoning, and readiness during timing simulation. S3 tests cover concurrent conditional writes and corruption detection. Live browser tests cover reconnecting with a recovery key and verified downloads through PostgreSQL, Redis Cluster, and MinIO.

See [docs/api.md](docs/api.md) for the API contract and [poc/README.md](poc/README.md) for the local proof of concept.
