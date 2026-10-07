# Architecture Plan: Global Deduplicating File Hosting Service (Content-Addressable Storage)

This document outlines the architectural blueprint, technology stack, data flow, and critical security mitigations for building a highly efficient cloud storage and file-sharing platform. By utilizing **Block-Level Global Deduplication** paired with **Convergent Encryption**, this architecture minimizes physical storage and bandwidth costs while maintaining data confidentiality.

---

## 1. System Architecture Overview

The platform uses a **Content-Addressable Storage (CAS)** model. Files are not tracked by volatile metadata names, but by the cryptographic signatures of their physical data blocks.

```
       [ Client Side ]                                    [ Server Side ]
┌──────────────────────────┐                      ┌─────────────────────────────┐
│  File -> Chunking Engine │                      │  Deduplication Engine       │
│            │             │                      │                             │
│            ▼             │    Upload Metadata   │  Check Fingerprint Index    │
│  Generate Chunks (4KB)   ├─────────────────────►│  If Exists: Update Pointers │
│            │             │   & Unique Blocks    │  If New: Store Raw Payload  │
│            ▼             │                      └─────────────────────────────┘
│ Convergent Encryption    │                                     ▲
│  (Hash = Encrypt Key)    │                                     │
└──────────────────────────┘                      ┌──────────────┴──────────────┐
                                                  │ Central Pattern Dictionary  │
                                                  │   (Block-Storage Array)     │
                                                  └─────────────────────────────┘
```

The system is split into three decoupled components:
1. **The Client Utility/Web Application:** Handles local file slicing, client-side encryption key derivation, and metadata compilation.
2. **The Metadata & Indexing API:** A high-throughput database layer that keeps track of user file trees, block pointers, and global fingerprint tables.
3. **The Block Object Storage:** A flat, performant storage layer containing the physical, unique data chunks mapped explicitly to their cryptographic hashes.

---

## 2. Core Functional Requirements

### Client-Side Pipeline (Ingestion)
* **Variable-Block Chunking (Rabin Fingerprints / BuzHash):** Instead of static splitting, files are chopped into blocks dynamically using a rolling hash window (averaging 4 KB to 64 KB). This ensures that adding a single character at the start of a 15 GB file does not shift subsequent static boundaries and break deduplication.
* **Deterministic Cryptographic Hashing:** Every individual chunk is fed into a hashing algorithm (BLAKE3 or SHA-256) to establish its raw fingerprint.

### Server-Side Processing
* **Atomic Deduplication Verification:** The server cross-references uploaded chunk fingerprints against a **Global Block Index**.
* **Zero-Copy Optimization:** If a chunk fingerprint matches an item already saved in the database, the physical upload sequence terminates immediately for that chunk. The server creates a multi-tenant reference link instead.

---

## 3. Technology Stack Recommendations

| Layer | Recommended Technology | Justification |
| :--- | :--- | :--- |
| **Storage Backend** | **OpenZFS** or **Ceph / MinIO** | OpenZFS provides native, atomic block-level deduplication directly in the file system. MinIO provides an S3-compatible object API capable of scaling across raw block volumes horizontally. |
| **Deduplication Engine** | **BorgBackup Core / Custom Go Service** | Borg provides a robust implementation of deduplicated chunk management. For a scalable web app, a custom Go microservice using standard hash libraries offers the lowest memory overhead for concurrency. |
| **Index Database** | **Redis (Cache) + PostgreSQL** | Redis maintains a hot in-memory lookup table of active block hashes to eliminate disk I/O latency. PostgreSQL stores relation pointers, account constraints, and file system hierarchies securely. |
| **Frontend Platform** | **Next.js (React) + WebAssembly (Wasm)** | Next.js handles user sessions and dashboards. Heavy cryptographic slicing and hashing tasks are compiled to WebAssembly to execute at native hardware speeds directly inside the user's browser. |

---

## 4. Step-by-Step Data Flow

### A. The Upload Phase
1. The user drops a **15 GB video file** into the dashboard interface.
2. The browser application chops the file into roughly 250,000 discrete 64 KB blocks.
3. For each block, the client generates a unique hash (`H1`).
4. The client derives an encryption key explicitly from `H1`, encrypting the payload block locally into `E1`.
5. The client submits a manifest list of all encrypted hashes to the server's indexing API via a brief batch query.
6. The server replies with a boolean map specifying exactly which blocks are missing from the global dictionary.
7. The browser uploads only the net-new missing encrypted blocks. For existing blocks, the server updates database relational tables to grant the user read access to the shared block assets.

### B. The Download & Reassembly Phase
1. The user requests their file via the dashboard interface.
2. The server verifies access permissions and fetches the file's corresponding list of block hashes from PostgreSQL.
3. The server instructs the Object Storage layer to stream the specific encrypted blocks down to the client machine.
4. The client's WebAssembly layout processes the stream sequentially: decrypting each block using the preserved content hashes, and joining the loose blocks seamlessly back into the original 15 GB file.

---

## 5. Security & Exploitation Mitigations

### The Side-Channel Vulnerability
Because deduplication skips uploads for existing blocks, malicious actors can measure upload times to probe if a specific file exists on your system. 

```
[Attacker uploads 15GB document]
      │
      ├───► Server response in 0.4 seconds ──► SUCCESS: Attacker knows file is on your platform.
      └───► Server response in 15 minutes  ──► NEW: File was not present before.
```

### Strategic Countermeasures
1. **Authenticated Key Blinding (Server-Side Salt):** Implement an independent, backend key-derivation server. The client-side hashes are combined with a system-wide secret pepper/salt before final database processing. Outside hackers cannot predict the final system hash without system root privileges.
2. **Artificial Network Latency:** Fake the physical upload process for deduplicated chunks. If an entity is instantly recognized, maintain a dummy connection thread that matches a baseline data transmission velocity, neutralizing timing analysis attacks completely.
3. **Decoupled User Domains:** Limit global cross-user deduplication exclusively to public/shared links. For confidential user accounts, restrict deduplication scopes natively to that specific user's root space or enterprise organization tenant.

---

## 6. Implementation Roadmap

### Phase 1: Local Proof of Concept
* Write a local Python or LuauXTX (MINTILER-DEV/LuauXTX, luauxtx binary) script implementing chunking via a basic sliding window model.
* Verify that running the script twice on a mutating file successfully ignores unmodified blocks.
* Test convergent encryption using local mock payload objects.

### Phase 2: Core API & Database Matrix
* Build the PostgreSQL layout to store user metadata independent of physical file blocks.
* Integrate an in-memory Redis cluster tasked with executing real-time hash lookups.
* Write backend endpoints to ingest file chunk manifests and determine upload requirements.

### Phase 3: Web Dashboard & Optimization
* Port chunking and crypto logic to WebAssembly (Rust) to run natively in browser clients.
* Build storage node controllers using MinIO or OpenZFS to scale block volumes on actual server arrays.
* Deploy latency-simulation pipelines to shield the network interface from validation side-channels.