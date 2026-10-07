# Phase 1: local deduplicating storage proof of concept

This implements phase 1 of `local/plan.md` in Python 3.10+:

- A BuzHash sliding window finds content-defined chunk boundaries. Defaults are
  a 64-byte window, 2 KiB minimum, 8 KiB boundary target, and 32 KiB maximum.
  The target is a boundary probability, not an exact mean; the minimum and
  maximum also influence observed sizes. The final chunk may be smaller.
- SHA-256 fingerprints identify plaintext chunks. HKDF-SHA-256 derives an
  AES-256-GCM key from each fingerprint; a domain-separated hash derives its
  nonce. Identical plaintext yields identical authenticated ciphertext.
- A local mock object store addresses encrypted blocks by their ciphertext
  SHA-256. Repeated blocks reuse existing objects without writing them again,
  including across filenames, file versions, and separate script invocations.
- Client manifests retain ordered chunk references, plaintext fingerprints,
  lengths, chunking settings, and a whole-file hash. Restore authenticates and
  decrypts each chunk, verifies the whole file, then atomically replaces the
  requested destination. Earlier versions remain restorable.

From the repository root:

```bash
python3 -m venv poc/.venv
poc/.venv/bin/python -m pip install -r poc/requirements.txt
poc/.venv/bin/python poc/cas.py demo
poc/.venv/bin/python -m unittest discover -s poc -v
```

`demo` uses a deterministic 1 MiB binary fixture in a temporary directory. It
uploads the original file, uploads it unchanged, then inserts a prefix and
overwrites four bytes in the middle. It asserts zero new blocks for the
unchanged upload, at least 90% byte reuse for the mutation, and byte-for-byte
restoration of all three uploads. It prints JSON results and cleans up afterward.

To keep a local store and use your own files:

```bash
poc/.venv/bin/python poc/cas.py ingest /path/to/file --store poc/.store
# Copy manifest_id from the JSON response; repeat ingestion after editing.
poc/.venv/bin/python poc/cas.py restore MANIFEST_ID /path/to/restored-file --store poc/.store
```

Use the same store across runs. Ingest reports new and reused chunk counts,
new and reused plaintext bytes, and newly stored ciphertext bytes (including
the format header and authentication tag). Counts include repeated occurrences;
only the first occurrence of a new block writes an object. Chunking can be
configured with `--min-size`, `--avg-size`, `--max-size`, and `--window-size`;
keep these settings consistent across versions for useful deduplication.

Store layout:

```text
poc/.store/
  objects/<first-two-hash-characters>/<ciphertext-sha256>
  manifests/<manifest-sha256>.json
```

Only ciphertext is placed in `objects/`. Manifests are deliberately kept in
plaintext for this local client/server simulation. Their plaintext fingerprints
enable key derivation, so access to a manifest and its objects enables decryption.
Protect manifests as client secrets; this prototype does not provide user
authentication, private manifest storage, or production confidentiality.
Convergent encryption also exposes equality and permits guessing known chunks.
The phase 2/3 database, network API, account isolation, key blinding, and timing
mitigations are outside this phase. The format's salt/domain constants are public
format identifiers, not a secret server pepper.

Ingest reads and chunks the source incrementally; restore writes one block at a
time. The manifest occupies memory proportional to the number of chunks. Objects
are published atomically using local filesystem hard links; restores use atomic
rename. Unreferenced objects after an interrupted ingest are harmless but are not
garbage-collected. This is a local filesystem PoC, without crash recovery or
directory-fsync guarantees. Keep a source stable during ingestion if a consistent
snapshot is needed. Successful restore replaces an existing destination.
