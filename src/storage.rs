use std::{fs, io::Write, path::{Path, PathBuf}, sync::Arc};
use axum::body::Bytes;
use sha2::{Digest, Sha256};
use crate::model::valid_hash;
use object_store::{ObjectStore as RemoteStore, PutMode, PutOptions, aws::AmazonS3Builder, path::Path as ObjectPath};

pub fn digest(data: impl AsRef<[u8]>) -> String { hex::encode(Sha256::digest(data.as_ref())) }

#[derive(Clone)]
pub struct ObjectStore { root: Arc<PathBuf>, remote: Option<Arc<dyn RemoteStore>> }

impl ObjectStore {
    pub fn new(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = root.as_ref().join("objects");
        fs::create_dir_all(&root)?;
        Ok(Self { root: Arc::new(root),remote:None })
    }
    pub fn from_env(root: impl AsRef<Path>) -> Result<Self,Box<dyn std::error::Error>> {
        match std::env::var("STORAGE_BACKEND").unwrap_or_else(|_|"filesystem".into()).as_str() {
            "filesystem"=>Ok(Self::new(root)?),
            "s3"=> {
                let endpoint=std::env::var("S3_ENDPOINT")?;
                let bucket=std::env::var("S3_BUCKET")?;
                let remote=AmazonS3Builder::from_env().with_endpoint(&endpoint).with_bucket_name(bucket)
                    .with_region(std::env::var("AWS_REGION").unwrap_or_else(|_|"us-east-1".into()))
                    .with_virtual_hosted_style_request(false).with_allow_http(endpoint.starts_with("http://")).build()?;
                Ok(Self {root:Arc::new(PathBuf::new()),remote:Some(Arc::new(remote))})
            }
            _=>Err("STORAGE_BACKEND must be filesystem or s3".into()),
        }
    }
    pub fn remote(remote: Arc<dyn RemoteStore>)->Self {Self {root:Arc::new(PathBuf::new()),remote:Some(remote)}}
    fn remote_path(object_id: &str)->std::io::Result<ObjectPath> {
        valid_hash(object_id).map_err(|_|std::io::Error::other("invalid object identifier"))?;
        Ok(ObjectPath::from(format!("objects/{}/{}",&object_id[..2],object_id)))
    }
    async fn verify_remote(remote: &dyn RemoteStore,path: &ObjectPath,payload: &[u8])->std::io::Result<()> {
        let existing=remote.get(path).await.map_err(std::io::Error::other)?.bytes().await.map_err(std::io::Error::other)?;
        if existing!=payload {return Err(std::io::Error::other("existing ciphertext object is corrupt"));}
        Ok(())
    }
    fn path(&self, object_id: &str) -> PathBuf {
        assert!(valid_hash(object_id).is_ok());
        self.root.join(&object_id[..2]).join(object_id)
    }
    pub async fn put(&self, object_id: String, payload: Bytes) -> std::io::Result<bool> {
        if let Some(remote)=&self.remote {
            let path=Self::remote_path(&object_id)?;
            match remote.head(&path).await {
                Ok(_)=>{Self::verify_remote(remote.as_ref(),&path,&payload).await?;return Ok(false);}
                Err(object_store::Error::NotFound{..})=>{},
                Err(error)=>return Err(std::io::Error::other(error)),
            }
            let options=PutOptions {mode:PutMode::Create,..Default::default()};
            return match remote.put_opts(&path,payload.clone().into(),options).await {
                Ok(_)=>Ok(true),
                Err(object_store::Error::AlreadyExists{..})|Err(object_store::Error::Precondition{..})=> {
                    Self::verify_remote(remote.as_ref(),&path,&payload).await?;Ok(false)
                }
                Err(error)=>Err(std::io::Error::other(error)),
            };
        }
        let path = self.path(&object_id);
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            if path.exists() {
                if fs::read(&path)? != payload { return Err(std::io::Error::other("existing ciphertext object is corrupt")); }
                return Ok(false);
            }
            let directory = path.parent().unwrap();
            fs::create_dir_all(directory)?;
            fs::File::open(root.as_ref())?.sync_all()?;
            let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
            temporary.write_all(&payload)?;
            temporary.as_file().sync_all()?;
            let created = match fs::hard_link(temporary.path(), &path) {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if fs::read(&path)? != payload { return Err(std::io::Error::other("existing ciphertext object is corrupt")); }
                    false
                }
                Err(error) => return Err(error),
            };
            // Persist the entry before PostgreSQL marks the block available.
            fs::File::open(directory)?.sync_all()?;
            Ok(created)
        }).await.map_err(std::io::Error::other)?
    }
    pub async fn get(&self, object_id: String, expected_size: usize) -> std::io::Result<Vec<u8>> {
        if let Some(remote)=&self.remote {
            let path=Self::remote_path(&object_id)?;
            let result=remote.get(&path).await.map_err(std::io::Error::other)?;
            if result.meta.size!=expected_size as u64 {return Err(std::io::Error::other("ciphertext object size mismatch"));}
            let data=result.bytes().await.map_err(std::io::Error::other)?;
            if digest(&data)!=object_id {return Err(std::io::Error::other("ciphertext object hash mismatch"));}
            return Ok(data.to_vec());
        }
        let path = self.path(&object_id);
        tokio::task::spawn_blocking(move || {
            if fs::metadata(&path)?.len() != expected_size as u64 { return Err(std::io::Error::other("ciphertext object size mismatch")); }
            let data = fs::read(path)?;
            if digest(&data) != object_id { return Err(std::io::Error::other("ciphertext object hash mismatch")); }
            Ok(data)
        }).await.map_err(std::io::Error::other)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn conditional_remote_publication_and_integrity() {
        let remote=Arc::new(object_store::memory::InMemory::new());
        let store=ObjectStore::remote(remote.clone());
        let data=Bytes::from_static(b"encrypted mock object");let id=digest(&data);
        let (a,b)=tokio::join!(store.put(id.clone(),data.clone()),store.put(id.clone(),data.clone()));
        assert_ne!(a.unwrap(),b.unwrap());
        assert_eq!(store.get(id.clone(),data.len()).await.unwrap(),data);
        remote.put(&ObjectStore::remote_path(&id).unwrap(),Bytes::from_static(b"corrupt").into()).await.unwrap();
        assert!(store.put(id.clone(),data.clone()).await.is_err());
        assert!(store.get(id,data.len()).await.is_err());
    }
}
