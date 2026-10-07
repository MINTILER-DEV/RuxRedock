-- A pending manifest must not dictate immutable global object metadata.
-- Keep each client's declared length on its own ordered reference instead.
ALTER TABLE file_chunks ADD COLUMN size INTEGER;
UPDATE file_chunks c SET size=b.size FROM blocks b WHERE b.object_id=c.object_id;
ALTER TABLE file_chunks ALTER COLUMN size SET NOT NULL;
ALTER TABLE file_chunks ADD CONSTRAINT chunk_size_valid CHECK(size BETWEEN 1 AND 1048576);
