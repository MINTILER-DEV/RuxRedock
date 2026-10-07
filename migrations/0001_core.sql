-- PostgreSQL 15+: NULLS NOT DISTINCT makes root-level names unique too.
CREATE TABLE users (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255),
    quota_bytes BIGINT NOT NULL DEFAULT 107374182400 CHECK (quota_bytes >= 0),
    used_bytes BIGINT NOT NULL DEFAULT 0 CHECK (used_bytes >= 0 AND used_bytes <= quota_bytes),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE api_tokens (
    token_hash TEXT PRIMARY KEY CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX api_tokens_user ON api_tokens(user_id);

CREATE TABLE directories (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    parent_id UUID,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255 AND name NOT IN ('.', '..') AND name !~ '[/\\\x00-\x1f\x7f]'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (id, user_id),
    UNIQUE NULLS NOT DISTINCT (user_id, parent_id, name),
    FOREIGN KEY (parent_id, user_id) REFERENCES directories(id, user_id),
    CHECK (parent_id IS DISTINCT FROM id)
);

CREATE TABLE files (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    parent_id UUID,
    name TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 255 AND name NOT IN ('.', '..') AND name !~ '[/\\\x00-\x1f\x7f]'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (user_id, parent_id, name),
    FOREIGN KEY (parent_id, user_id) REFERENCES directories(id, user_id)
);

-- Global immutable ciphertext index. Pending rows never count as dedup hits.
CREATE TABLE blocks (
    object_id TEXT PRIMARY KEY CHECK (object_id ~ '^[0-9a-f]{64}$'),
    size INTEGER NOT NULL CHECK (size BETWEEN 1 AND 1048576),
    available BOOLEAN NOT NULL DEFAULT false,
    stored_at TIMESTAMPTZ,
    CHECK (available = (stored_at IS NOT NULL))
);

CREATE TABLE file_versions (
    id UUID PRIMARY KEY,
    file_id UUID NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    size BIGINT NOT NULL CHECK (size BETWEEN 0 AND 107374182400),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'ready')),
    -- Opaque encrypted client manifest: no convergent keys/plaintext hashes.
    client_metadata TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CHECK ((status = 'ready') = (completed_at IS NOT NULL))
);
CREATE INDEX versions_by_file ON file_versions(file_id, created_at DESC, id);

CREATE TABLE file_chunks (
    version_id UUID NOT NULL REFERENCES file_versions(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    object_id TEXT NOT NULL REFERENCES blocks(object_id),
    PRIMARY KEY (version_id, ordinal)
);
CREATE INDEX chunks_by_object ON file_chunks(object_id, version_id);

-- Reference counts derive from actual rows, rather than racing counters.
CREATE VIEW block_references AS
SELECT b.object_id, b.size, b.available, count(c.version_id) AS reference_count
FROM blocks b LEFT JOIN file_chunks c USING (object_id)
GROUP BY b.object_id;
