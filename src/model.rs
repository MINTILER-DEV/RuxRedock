use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::error::{ApiError, Result};

pub const MAX_BLOCK_SIZE: usize = 1024 * 1024;
pub const OBJECT_HEADER: &[u8] = b"RUXCAS\x01";
pub const OBJECT_OVERHEAD: usize = OBJECT_HEADER.len() + 16;
pub const MAX_CHUNKS: usize = 250_000;
pub const MAX_MANIFEST_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkSpec {
    pub object_id: String,
    pub size: i32,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub size: i64,
    pub client_metadata: String,
    pub chunks: Vec<ChunkSpec>,
}

impl Manifest {
    pub fn validate(&self) -> Result<()> {
        valid_name(&self.name)?;
        if self.chunks.len() > MAX_CHUNKS { return Err(ApiError::bad("too many chunks")); }
        if self.client_metadata.len() > 96 * 1024 * 1024 {
            return Err(ApiError::bad("client metadata exceeds 96 MiB"));
        }
        if !(0..=107_374_182_400).contains(&self.size) {
            return Err(ApiError::bad("file size must be between 0 and 100 GiB"));
        }
        let mut total = 0_i64;
        let mut seen = std::collections::HashMap::new();
        for chunk in &self.chunks {
            valid_hash(&chunk.object_id)?;
            if !(1..=MAX_BLOCK_SIZE as i32).contains(&chunk.size) {
                return Err(ApiError::bad("chunk size must be between 1 byte and 1 MiB"));
            }
            total += i64::from(chunk.size);
            if seen.insert(&chunk.object_id, chunk.size).is_some_and(|size| size != chunk.size) {
                return Err(ApiError::bad("one object identifier has conflicting sizes"));
            }
        }
        if total != self.size { return Err(ApiError::bad("chunk sizes do not match file size")); }
        Ok(())
    }
}

pub fn valid_hash(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(ApiError::bad("expected a lowercase SHA-256 identifier"));
    }
    Ok(())
}

pub fn valid_name(name: &str) -> Result<()> {
    if name.is_empty() || name.chars().count() > 255 || name == "." || name == ".."
        || name.chars().any(|c| c.is_control() || c == '/' || c == '\\') {
        return Err(ApiError::bad("name must contain 1–255 characters without separators or control characters"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectoryInput {
    pub name: String,
    pub parent_id: Option<Uuid>,
}

#[derive(Default, Deserialize)]
pub struct Listing {
    pub parent_id: Option<Uuid>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl Listing {
    pub fn pagination(&self) -> Result<(i64, i64)> {
        let limit = self.limit.unwrap_or(100);
        let offset = self.offset.unwrap_or(0);
        if !(1..=1000).contains(&limit) || offset < 0 {
            return Err(ApiError::bad("limit must be 1–1000 and offset nonnegative"));
        }
        Ok((limit, offset))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lookup {
    pub chunks: Vec<ChunkSpec>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_and_conflicting_manifests() {
        let mut manifest = Manifest { name: "test".into(), parent_id: None, size: 1,
            client_metadata: "".into(), chunks: vec![ChunkSpec {object_id: "a".repeat(64), size: 1}] };
        assert!(manifest.validate().is_ok());
        manifest.chunks.push(ChunkSpec {object_id: "a".repeat(64), size: 2});
        manifest.size = 3;
        assert!(manifest.validate().is_err());
        for name in ["", ".", "..", "../test", "a\\b", "a\0b"] { assert!(valid_name(name).is_err()); }
        assert!(valid_hash(&"A".repeat(64)).is_err());
    }
}
