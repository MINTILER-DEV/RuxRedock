#!/usr/bin/env python3
"""Local content-defined, convergently encrypted file storage proof of concept."""

from __future__ import annotations

import argparse
from collections import deque
from dataclasses import asdict, dataclass
import hashlib
import json
import os
from pathlib import Path
import random
import tempfile
from typing import BinaryIO, Iterator

from cryptography.exceptions import InvalidTag
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF


MASK64 = (1 << 64) - 1
TABLE = tuple(
    int.from_bytes(hashlib.sha256(b"ruxredock-buzhash-v1" + bytes([i])).digest()[:8], "big")
    for i in range(256)
)
DOMAIN = b"ruxredock-convergent-aes256gcm-v1"
OBJECT_HEADER = b"RUXCAS\x01"


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def rotate(value: int, count: int) -> int:
    count %= 64
    return ((value << count) | (value >> ((64 - count) % 64))) & MASK64


@dataclass(frozen=True)
class Chunking:
    min_size: int = 2048
    avg_size: int = 8192
    max_size: int = 32768
    window_size: int = 64

    def __post_init__(self) -> None:
        if not 0 < self.window_size <= self.min_size <= self.avg_size <= self.max_size:
            raise ValueError("require 0 < window <= min <= avg <= max")
        if self.avg_size & (self.avg_size - 1):
            raise ValueError("average chunk size must be a power of two")


def chunks(stream: BinaryIO, config: Chunking = Chunking()) -> Iterator[bytes]:
    """BuzHash boundaries depend on the last window_size bytes, not offsets.

    Retain the rolling window across chunk boundaries so an insertion can
    resynchronize with the original stream. min/max bound emitted block sizes;
    the last block may be shorter than min_size. Memory is bounded by max_size
    plus the read buffer and rolling window.
    """
    window: deque[int] = deque()
    rolling = 0
    pending = bytearray()
    while data := stream.read(65536):
        for byte in data:
            rolling = rotate(rolling, 1) ^ TABLE[byte]
            if len(window) == config.window_size:
                rolling ^= rotate(TABLE[window.popleft()], config.window_size)
            window.append(byte)
            pending.append(byte)
            if len(pending) >= config.max_size or (
                len(pending) >= config.min_size and rolling & (config.avg_size - 1) == 0
            ):
                yield bytes(pending)
                pending.clear()
    if pending:
        yield bytes(pending)


def crypto_material(fingerprint: str) -> tuple[bytes, bytes, bytes]:
    digest = bytes.fromhex(valid_hash(fingerprint))
    key = HKDF(
        algorithm=hashes.SHA256(), length=32, salt=DOMAIN, info=b"chunk-key"
    ).derive(digest)
    nonce = hashlib.sha256(DOMAIN + b"nonce" + digest).digest()[:12]
    return key, nonce, OBJECT_HEADER + digest


def encrypt_chunk(plaintext: bytes) -> tuple[str, str, bytes]:
    fingerprint = sha256(plaintext)
    key, nonce, aad = crypto_material(fingerprint)
    payload = OBJECT_HEADER + AESGCM(key).encrypt(nonce, plaintext, aad)
    return fingerprint, sha256(payload), payload


def decrypt_chunk(payload: bytes, fingerprint: str, object_id: str) -> bytes:
    if sha256(payload) != valid_hash(object_id):
        raise ValueError("encrypted object hash mismatch")
    if not payload.startswith(OBJECT_HEADER):
        raise ValueError("unsupported encrypted object format")
    key, nonce, aad = crypto_material(fingerprint)
    try:
        plaintext = AESGCM(key).decrypt(nonce, payload[len(OBJECT_HEADER):], aad)
    except InvalidTag as exc:
        raise ValueError("encrypted object authentication failed") from exc
    if sha256(plaintext) != fingerprint:
        raise ValueError("decrypted chunk fingerprint mismatch")
    return plaintext


def valid_hash(value: str) -> str:
    if not isinstance(value, str) or len(value) != 64 or any(c not in "0123456789abcdef" for c in value):
        raise ValueError("expected a lowercase SHA-256 identifier")
    return value


def canonical_json(value: dict) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def write_once(path: Path, data: bytes) -> bool:
    """Publish a complete object atomically, without replacing an existing one."""
    if path.exists():
        if path.read_bytes() != data:
            raise ValueError(f"existing content-addressed object is corrupt: {path.name}")
        return False
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".pending-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            os.link(temporary, path)
            return True
        except FileExistsError:
            if path.read_bytes() != data:
                raise ValueError(f"existing content-addressed object is corrupt: {path.name}")
            return False
    finally:
        os.unlink(temporary)


class Store:
    """Mock object storage: encrypted blocks and client-owned JSON manifests."""

    def __init__(self, root: Path):
        self.root = Path(root)

    def object_path(self, object_id: str) -> Path:
        valid_hash(object_id)
        return self.root / "objects" / object_id[:2] / object_id

    def manifest_path(self, manifest_id: str) -> Path:
        return self.root / "manifests" / (valid_hash(manifest_id) + ".json")

    def ingest(self, source: Path, config: Chunking = Chunking()) -> dict:
        file_hash = hashlib.sha256()
        manifest = {
            "version": 1,
            "encryption": "convergent-aes256gcm-hkdf-sha256-v1",
            "name": source.name,
            "chunking": asdict(config),
            "size": 0,
            "chunks": [],
        }
        new_chunks = reused_chunks = new_bytes = reused_bytes = stored_bytes = 0
        with source.open("rb") as stream:
            for plaintext in chunks(stream, config):
                fingerprint, object_id, payload = encrypt_chunk(plaintext)
                created = write_once(self.object_path(object_id), payload)
                if created:
                    new_chunks += 1
                    new_bytes += len(plaintext)
                    stored_bytes += len(payload)
                else:
                    reused_chunks += 1
                    reused_bytes += len(plaintext)
                file_hash.update(plaintext)
                manifest["size"] += len(plaintext)
                manifest["chunks"].append({
                    "fingerprint": fingerprint, "object_id": object_id, "size": len(plaintext)
                })
        manifest["sha256"] = file_hash.hexdigest()
        encoded = canonical_json(manifest)
        manifest_id = sha256(encoded)
        write_once(self.manifest_path(manifest_id), encoded)
        return {
            "manifest_id": manifest_id,
            "file_bytes": manifest["size"],
            "total_chunks": len(manifest["chunks"]),
            "new_chunks": new_chunks,
            "reused_chunks": reused_chunks,
            "new_plaintext_bytes": new_bytes,
            "reused_plaintext_bytes": reused_bytes,
            "new_stored_bytes": stored_bytes,
        }

    def read_manifest(self, manifest_id: str) -> dict:
        encoded = self.manifest_path(manifest_id).read_bytes()
        if sha256(encoded) != manifest_id:
            raise ValueError("manifest hash mismatch")
        manifest = json.loads(encoded)
        if not isinstance(manifest, dict) or manifest.get("version") != 1:
            raise ValueError("unsupported manifest format")
        if manifest.get("encryption") != "convergent-aes256gcm-hkdf-sha256-v1":
            raise ValueError("unsupported encryption format")
        valid_hash(manifest.get("sha256"))
        config = Chunking(**manifest["chunking"])
        if type(manifest["size"]) is not int or manifest["size"] < 0:
            raise ValueError("invalid file size")
        if not isinstance(manifest["chunks"], list):
            raise ValueError("invalid chunk list")
        total = 0
        for block in manifest["chunks"]:
            valid_hash(block["fingerprint"])
            valid_hash(block["object_id"])
            if type(block["size"]) is not int or not 0 < block["size"] <= config.max_size:
                raise ValueError("invalid chunk size")
            total += block["size"]
        if total != manifest["size"]:
            raise ValueError("manifest chunk sizes do not match file size")
        return manifest

    def restore(self, manifest_id: str, destination: Path) -> dict:
        manifest = self.read_manifest(manifest_id)
        destination = Path(destination)
        destination.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary = tempfile.mkstemp(prefix=".restore-", dir=destination.parent)
        restored_hash = hashlib.sha256()
        try:
            with os.fdopen(fd, "wb") as stream:
                for block in manifest["chunks"]:
                    with self.object_path(block["object_id"]).open("rb") as obj:
                        payload = obj.read(block["size"] + len(OBJECT_HEADER) + 16 + 1)
                    if len(payload) != block["size"] + len(OBJECT_HEADER) + 16:
                        raise ValueError("encrypted object size mismatch")
                    plaintext = decrypt_chunk(payload, block["fingerprint"], block["object_id"])
                    stream.write(plaintext)
                    restored_hash.update(plaintext)
                if restored_hash.hexdigest() != manifest["sha256"]:
                    raise ValueError("restored file hash mismatch")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, destination)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        return {"destination": str(destination), "file_bytes": manifest["size"], "sha256": manifest["sha256"]}


def demo() -> dict:
    """Three ingests plus verified restores, in a disposable isolated store."""
    with tempfile.TemporaryDirectory(prefix="ruxredock-demo-") as directory:
        root = Path(directory)
        source = root / "sample.bin"
        original = random.Random(2026).randbytes(1024 * 1024)
        source.write_bytes(original)
        store = Store(root / "store")
        first = store.ingest(source)
        second = store.ingest(source)
        midpoint = len(original) // 2
        modified = b"Inserted prefix to shift every original offset.\n" + original[:midpoint] + b"EDIT" + original[midpoint + 4:]
        source.write_bytes(modified)
        third = store.ingest(source)
        if second["new_chunks"] != 0 or second["reused_plaintext_bytes"] != len(original):
            raise AssertionError("identical upload did not reuse all blocks")
        if third["new_chunks"] == 0 or third["reused_plaintext_bytes"] < len(original) * 0.9:
            raise AssertionError("mutation did not reuse at least 90% of the original bytes")
        for index, (result, expected) in enumerate(((first, original), (second, original), (third, modified))):
            target = root / f"restored-{index}.bin"
            store.restore(result["manifest_id"], target)
            if target.read_bytes() != expected:
                raise AssertionError("restored file differs from original")
        return {"first_upload": first, "identical_upload": second, "mutated_upload": third,
                "mutation_reuse_percent": round(100 * third["reused_plaintext_bytes"] / len(modified), 2),
                "round_trips_verified": 3}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    ingest = commands.add_parser("ingest", help="chunk, encrypt, and store only new blocks")
    ingest.add_argument("source", type=Path)
    ingest.add_argument("--store", type=Path, default=Path("poc/.store"))
    ingest.add_argument("--min-size", type=int, default=2048)
    ingest.add_argument("--avg-size", type=int, default=8192)
    ingest.add_argument("--max-size", type=int, default=32768)
    ingest.add_argument("--window-size", type=int, default=64)
    restore = commands.add_parser("restore", help="authenticate, decrypt, and reassemble a file")
    restore.add_argument("manifest_id")
    restore.add_argument("destination", type=Path)
    restore.add_argument("--store", type=Path, default=Path("poc/.store"))
    commands.add_parser("demo", help="verify repeat uploads and reuse after insertion and overwrite")
    args = parser.parse_args()
    try:
        if args.command == "ingest":
            result = Store(args.store).ingest(args.source, Chunking(args.min_size, args.avg_size, args.max_size, args.window_size))
        elif args.command == "restore":
            result = Store(args.store).restore(args.manifest_id, args.destination)
        else:
            result = demo()
    except (OSError, ValueError, KeyError, TypeError) as exc:
        parser.exit(1, f"error: {exc}\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
