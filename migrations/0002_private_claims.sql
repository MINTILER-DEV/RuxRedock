-- Private accounts must possess ciphertext before reusing a global object.
CREATE TABLE block_claims (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    object_id TEXT NOT NULL REFERENCES blocks(object_id),
    PRIMARY KEY(user_id,object_id)
);
CREATE INDEX claims_by_object ON block_claims(object_id);
-- Existing ready versions are proof of possession from the phase 2 API.
INSERT INTO block_claims(user_id,object_id)
SELECT DISTINCT f.user_id,c.object_id FROM files f
JOIN file_versions v ON v.file_id=f.id JOIN file_chunks c ON c.version_id=v.id
JOIN blocks b ON b.object_id=c.object_id WHERE v.status='ready' AND b.available
ON CONFLICT DO NOTHING;
