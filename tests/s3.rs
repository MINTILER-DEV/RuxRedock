use RuxRedock::storage::{ObjectStore, digest};
use axum::body::Bytes;
use object_store::{ObjectStore as RemoteStore, aws::AmazonS3Builder, path::Path};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires a pre-provisioned MinIO bucket and S3 environment variables"]
async fn real_minio_conditional_writes_reads_and_corruption_detection() {
    let endpoint = std::env::var("S3_ENDPOINT").expect("S3_ENDPOINT is required");
    let bucket = std::env::var("S3_BUCKET").expect("S3_BUCKET is required");
    let remote = Arc::new(
        AmazonS3Builder::from_env()
            .with_endpoint(&endpoint)
            .with_bucket_name(bucket)
            .with_region("us-east-1")
            .with_virtual_hosted_style_request(false)
            .with_allow_http(endpoint.starts_with("http://"))
            .build()
            .unwrap(),
    );
    let store = ObjectStore::remote(remote.clone());
    let payload = Bytes::from(format!(
        "encrypted integration payload {}",
        uuid::Uuid::new_v4()
    ));
    let id = digest(&payload);
    let (first, second) = tokio::join!(
        store.put(id.clone(), payload.clone()),
        store.put(id.clone(), payload.clone())
    );
    assert_ne!(first.unwrap(), second.unwrap());
    assert_eq!(store.get(id.clone(), payload.len()).await.unwrap(), payload);
    assert!(!store.put(id.clone(), payload.clone()).await.unwrap());
    let path = Path::from(format!("objects/{}/{}", &id[..2], id));
    remote
        .put(&path, Bytes::from_static(b"corrupt").into())
        .await
        .unwrap();
    assert!(store.get(id.clone(), payload.len()).await.is_err());
    assert!(store.put(id, payload).await.is_err());
    remote.delete(&path).await.unwrap();
}
