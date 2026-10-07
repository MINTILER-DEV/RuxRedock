# API

All `/v1` requests require `Authorization: Bearer TOKEN`. Create accounts locally with `RuxRedock create-user NAME [QUOTA_BYTES]`; only a SHA-256 token hash is stored in PostgreSQL. Authentication failures return 401; missing or inaccessible resources return 404. JSON application errors contain `error`, `message`, and optional `details`. `GET /health` is public and checks PostgreSQL.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/v1/me` | Account name, identifier, quota and logical usage |
| GET/POST | `/v1/directories` | List/create directories |
| PATCH/DELETE | `/v1/directories/{id}` | Rename/delete an empty directory |
| GET/POST | `/v1/files` | List files/ingest an upload manifest |
| GET/PATCH/DELETE | `/v1/files/{id}` | Metadata/rename/delete all versions and release quota |
| GET | `/v1/files/{id}/versions` | List pending and ready versions, newest first |
| GET | `/v1/files/{id}/versions/{version}` | Sealed client metadata and ordered chunk references |
| GET/DELETE | `/v1/uploads/{version}` | Remaining requirements/cancel a pending upload |
| PUT | `/v1/uploads/{version}/blocks/{hash}` | Upload referenced encrypted block bytes |
| POST | `/v1/uploads/{version}/complete` | Validate available blocks, apply timing simulation, mark ready |
| POST | `/v1/blocks/check` | Batch lookup within the configured deduplication scope |
| GET | `/v1/blocks/{hash}` | Read ciphertext referenced by an owned, ready version |

Listings accept `limit` (1–1000, default 100) and `offset` (default 0). Files/directories also accept `parent_id`; omit it for the root. Responses wrap lists under `files`, `directories`, or `versions`. File entries include `size`, `modified`, `version_count`, and `status`. Names contain 1–255 characters without path separators or control characters. Folder and file names share a namespace within a parent.

Create a directory with `{"name":"Projects","parent_id":null}`. Rename with `{"name":"New name"}`. Empty directories only may be deleted. Root is represented by null, not a synthetic directory identifier.

## Upload

POST `/v1/files` with:

```json
{
  "name": "example.bin",
  "parent_id": null,
  "size": 3,
  "client_metadata": "opaque encrypted manifest",
  "chunks": [
    {"object_id": "<64 lowercase hexadecimal ciphertext SHA-256 characters>", "size": 3}
  ]
}
```

Chunk `size` is the **plaintext** length; the ciphertext has an additional 23-byte format/header and authentication tag. The sum of ordered chunk sizes must match file size. Duplicate references are allowed. Empty files have size zero and an empty chunk list. The API treats `client_metadata` as opaque; the browser seals its private manifest using AES-GCM under the recovery key.

The response includes `file_id`, `version_id`, `status`, `missing` (booleans in input order), `missing_chunks` (unique required objects), and `upload_bytes` (ciphertext bytes required). Ingesting the same name in the same folder creates a new version. Each version reserves logical quota atomically before upload.

PUT raw ciphertext to each required block URL using `Content-Type: application/octet-stream`. The server verifies the format header, ciphertext length, and ciphertext SHA-256, writes it durably, then records availability and the account's proof of possession. Repeated uploads are idempotent. Unfinished manifests cannot dictate a shared object's canonical length.

POST `{}` to the completion endpoint. A 409 with `details.missing_object_ids` means blocks are still missing or a cached requirement was stale; upload those blocks and retry. Once complete, the version becomes downloadable. The readiness change occurs only after the baseline simulation delay. A later GET upload status can recover requirements after an interrupted transfer; DELETE cancels pending state and releases quota. Blocks already written may remain physically stored.

POST `/v1/blocks/check` accepts `{"chunks":[{"object_id":"...","size":3}]}`. Tenant mode checks that account's supplied ciphertext; global mode checks shared availability. This lookup alone grants no read authorization. Ciphertext downloads always require an owned ready reference.

## Restore

GET the selected version's manifest. Decrypt its `client_metadata` with the recovery key to obtain ordered plaintext fingerprints and the whole-file SHA-256. Download each encrypted object, verify its ciphertext identifier, derive the convergent key from its private plaintext fingerprint, authenticate/decrypt, and reassemble in order. Verify each plaintext fingerprint, lengths, and the whole-file hash before committing output. The dashboard implements this in its worker and streaming/Blob download paths.
