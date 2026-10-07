"""Phase 1 acceptance tests. Run: python3 -m unittest discover -s poc -v."""

import hashlib
import io
import json
from pathlib import Path
import random
import subprocess
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

from cas import (
    Chunking, OBJECT_HEADER, Store, canonical_json, chunks, decrypt_chunk,
    demo, encrypt_chunk, sha256, write_once,
)


class ChunkingTests(unittest.TestCase):
    def test_boundaries_are_deterministic_bounded_and_lossless(self):
        data = random.Random(10).randbytes(512 * 1024)
        config = Chunking()
        result = list(chunks(io.BytesIO(data), config))
        self.assertEqual(b"".join(result), data)
        self.assertEqual(result, list(chunks(io.BytesIO(data), config)))
        self.assertTrue(all(config.min_size <= len(part) <= config.max_size for part in result[:-1]))
        self.assertLessEqual(len(result[-1]), config.max_size)
        self.assertGreater(len(result), 20)
        self.assertGreater(len({len(part) for part in result}), 10)

    def test_stream_read_boundaries_do_not_change_chunks(self):
        class ShortReads(io.BytesIO):
            def read(self, size=-1):
                return super().read(min(size, 137))

        data = random.Random(11).randbytes(128 * 1024)
        self.assertEqual(list(chunks(io.BytesIO(data))), list(chunks(ShortReads(data))))

    def test_empty_short_and_forced_maximum(self):
        self.assertEqual(list(chunks(io.BytesIO(b""))), [])
        self.assertEqual(list(chunks(io.BytesIO(b"short"))), [b"short"])
        config = Chunking(min_size=8, avg_size=16, max_size=16, window_size=4)
        result = list(chunks(io.BytesIO(bytes(range(255))), config))
        self.assertEqual(b"".join(result), bytes(range(255)))
        self.assertTrue(all(len(part) <= 16 for part in result))

    def test_invalid_configuration(self):
        for options in ({"avg_size": 9000}, {"min_size": 0}, {"window_size": 3000}, {"max_size": 4096}):
            with self.subTest(options=options), self.assertRaises(ValueError):
                Chunking(**options)


class EncryptionTests(unittest.TestCase):
    def test_convergence_round_trip_and_distinct_content(self):
        plaintext = b"local mock payload object\x00" * 100
        fingerprint, object_id, payload = encrypt_chunk(plaintext)
        self.assertEqual((fingerprint, object_id, payload), encrypt_chunk(plaintext))
        self.assertNotEqual(object_id, encrypt_chunk(plaintext + b"!")[1])
        self.assertNotIn(plaintext, payload)
        self.assertEqual(len(payload), len(plaintext) + len(OBJECT_HEADER) + 16)
        self.assertEqual(decrypt_chunk(payload, fingerprint, object_id), plaintext)

    def test_tampering_and_wrong_key_are_rejected(self):
        fingerprint, object_id, payload = encrypt_chunk(b"test payload")
        damaged = payload[:-1] + bytes([payload[-1] ^ 1])
        with self.assertRaisesRegex(ValueError, "object hash mismatch"):
            decrypt_chunk(damaged, fingerprint, object_id)
        # Even when a ciphertext hash is recomputed, the AEAD tag rejects edits.
        with self.assertRaisesRegex(ValueError, "authentication failed"):
            decrypt_chunk(damaged, fingerprint, sha256(damaged))
        with self.assertRaisesRegex(ValueError, "authentication failed"):
            decrypt_chunk(payload, sha256(b"different plaintext"), object_id)


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.store = Store(self.root / "store")
        self.source = self.root / "source.bin"

    def ingest(self, data):
        self.source.write_bytes(data)
        return self.store.ingest(self.source)

    def test_repeat_ingestion_performs_no_object_writes(self):
        data = random.Random(12).randbytes(128 * 1024)
        first = self.ingest(data)
        paths = list((self.store.root / "objects").rglob("*"))
        timestamps = {path: path.stat().st_mtime_ns for path in paths if path.is_file()}
        with patch("cas.tempfile.mkstemp", side_effect=AssertionError("duplicate write")):
            second = self.store.ingest(self.source)
        self.assertEqual(first["manifest_id"], second["manifest_id"])
        self.assertEqual(second["new_chunks"], 0)
        self.assertEqual(second["new_stored_bytes"], 0)
        self.assertEqual(second["reused_plaintext_bytes"], len(data))
        self.assertEqual(timestamps, {path: path.stat().st_mtime_ns for path in timestamps})

    def test_empty_and_binary_round_trips(self):
        for data in (b"", b"x", bytes(range(256)) * 500):
            with self.subTest(size=len(data)):
                result = self.ingest(data)
                target = self.root / "restored.bin"
                self.store.restore(result["manifest_id"], target)
                self.assertEqual(target.read_bytes(), data)

    def test_repeated_blocks_within_file_share_one_object(self):
        data = b"\x00" * (2048 * 10)
        self.source.write_bytes(data)
        # Constant input need not hit a rolling-hash boundary. Force equal-sized
        # blocks here to isolate within-file deduplication from boundary choice.
        result = self.store.ingest(self.source, Chunking(min_size=2048, avg_size=2048, max_size=2048))
        self.assertEqual(result["new_chunks"], 1)
        self.assertEqual(result["reused_chunks"], 9)
        self.assertEqual(len(list((self.store.root / "objects").rglob("[0-9a-f]" * 64))), 1)
        target = self.root / "restored.bin"
        self.store.restore(result["manifest_id"], target)
        self.assertEqual(target.read_bytes(), data)

    def test_deduplication_across_different_filenames_and_store_instances(self):
        data = random.Random(13).randbytes(64000)
        first = self.ingest(data)
        other = self.root / "other.bin"
        other.write_bytes(data)
        second = Store(self.store.root).ingest(other)
        self.assertEqual(second["new_chunks"], 0)
        self.assertNotEqual(first["manifest_id"], second["manifest_id"])

    def test_corruption_and_missing_blocks_preserve_destination(self):
        result = self.ingest(b"plaintext data")
        manifest = self.store.read_manifest(result["manifest_id"])
        obj = self.store.object_path(manifest["chunks"][0]["object_id"])
        target = self.root / "output.bin"
        target.write_bytes(b"previous output")
        payload = obj.read_bytes()
        obj.write_bytes(payload[:-1] + bytes([payload[-1] ^ 1]))
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            self.store.restore(result["manifest_id"], target)
        self.assertEqual(target.read_bytes(), b"previous output")
        with self.assertRaisesRegex(ValueError, "corrupt"):
            self.store.ingest(self.source)
        obj.unlink()
        with self.assertRaises(FileNotFoundError):
            self.store.restore(result["manifest_id"], target)
        self.assertEqual(target.read_bytes(), b"previous output")
        self.assertEqual(list(self.root.glob(".restore-*")), [])

    def test_manifest_corruption_invalid_ids_and_bad_metadata(self):
        result = self.ingest(b"test")
        path = self.store.manifest_path(result["manifest_id"])
        path.write_text("{}")
        with self.assertRaisesRegex(ValueError, "manifest hash mismatch"):
            self.store.read_manifest(result["manifest_id"])
        with self.assertRaises(ValueError):
            self.store.read_manifest("../../outside")
        result = self.ingest(b"new test")
        manifest = self.store.read_manifest(result["manifest_id"])
        manifest["size"] += 1
        encoded = canonical_json(manifest)
        manifest_id = sha256(encoded)
        self.store.manifest_path(manifest_id).write_bytes(encoded)
        with self.assertRaisesRegex(ValueError, "sizes do not match"):
            self.store.read_manifest(manifest_id)

    def test_wrong_whole_file_hash_preserves_destination(self):
        result = self.ingest(b"test")
        manifest = self.store.read_manifest(result["manifest_id"])
        manifest["sha256"] = hashlib.sha256(b"wrong").hexdigest()
        encoded = canonical_json(manifest)
        manifest_id = sha256(encoded)
        self.store.manifest_path(manifest_id).write_bytes(encoded)
        target = self.root / "output.bin"
        target.write_bytes(b"previous output")
        with self.assertRaisesRegex(ValueError, "restored file hash mismatch"):
            self.store.restore(manifest_id, target)
        self.assertEqual(target.read_bytes(), b"previous output")

    def test_atomic_publication_under_concurrent_writers(self):
        path = self.root / "object"
        payload = random.Random(14).randbytes(10000)
        with ThreadPoolExecutor(max_workers=4) as executor:
            created = list(executor.map(lambda _: write_once(path, payload), range(8)))
        self.assertEqual(sum(created), 1)
        self.assertEqual(path.read_bytes(), payload)
        self.assertEqual(list(self.root.glob(".pending-*")), [])

    def test_mutation_demo_reuses_blocks_and_preserves_versions(self):
        result = demo()
        self.assertEqual(result["identical_upload"]["new_chunks"], 0)
        self.assertGreaterEqual(result["mutation_reuse_percent"], 90)
        self.assertGreater(result["mutated_upload"]["new_chunks"], 0)
        self.assertEqual(result["round_trips_verified"], 3)

    def test_cli_persistence_restore_and_error_reporting(self):
        script = str(Path(__file__).with_name("cas.py"))

        def run(*arguments):
            return subprocess.run(
                [sys.executable, script, *map(str, arguments), "--store", str(self.store.root)],
                capture_output=True, text=True,
            )

        data = random.Random(15).randbytes(48000)
        self.source.write_bytes(data)
        first = run("ingest", self.source)
        self.assertEqual(first.returncode, 0, first.stderr)
        manifest_id = json.loads(first.stdout)["manifest_id"]
        second = run("ingest", self.source)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(json.loads(second.stdout)["new_chunks"], 0)
        target = self.root / "cli-restored.bin"
        restored = run("restore", manifest_id, target)
        self.assertEqual(restored.returncode, 0, restored.stderr)
        self.assertEqual(target.read_bytes(), data)
        invalid = run("restore", "../bad-id", target)
        self.assertEqual(invalid.returncode, 1)
        self.assertIn("expected a lowercase SHA-256 identifier", invalid.stderr)
        self.assertNotIn("Traceback", invalid.stderr)
        self.assertEqual(target.read_bytes(), data)


if __name__ == "__main__":
    unittest.main()
