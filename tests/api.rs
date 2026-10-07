//! Real PostgreSQL/Redis tests; opt in with `cargo test --test api -- --ignored`.
use std::env;
use axum::{Router, body::{Body, to_bytes}, http::{Request, StatusCode}};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;
use RuxRedock::{AppState, MIGRATOR, cache::BlockCache, create_user, model::{ChunkSpec, OBJECT_HEADER, OBJECT_OVERHEAD}, router, storage::{ObjectStore, digest}};

struct Harness { state: AppState, app: Router, admin: PgPool, schema: String, _directory: TempDir }

impl Harness {
    async fn new() -> Self {
        let url = env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must point to a test PostgreSQL database");
        let admin = PgPool::connect(&url).await.unwrap();
        let schema = format!("test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&admin).await.unwrap();
        let search_path = schema.clone();
        let db = PgPoolOptions::new().max_connections(10).after_connect(move |conn,_| {
            let query = format!("SET search_path TO {search_path}");
            Box::pin(async move { sqlx::query(&query).execute(conn).await?; Ok(()) })
        }).connect(&url).await.unwrap();
        MIGRATOR.run(&db).await.unwrap();
        let urls: Vec<String> = env::var("TEST_REDIS_URLS").unwrap_or_else(|_| "redis://127.0.0.1:6379".into())
            .split(',').map(str::to_owned).collect();
        let cluster = env::var("TEST_REDIS_CLUSTER").unwrap_or_else(|_| "false".into()) == "true";
        let directory = tempfile::tempdir().unwrap();
        let state = AppState {db,cache:BlockCache::new(&urls,cluster,&schema).unwrap(),storage:ObjectStore::new(directory.path()).unwrap(),policy:RuxRedock::policy::UploadPolicy{global:true,minimum_response:std::time::Duration::ZERO,bytes_per_second:u64::MAX}};
        assert!(state.cache.healthy().await, "the integration suite requires live Redis");
        let app = router(state.clone());
        Self {state,app,admin,schema,_directory:directory}
    }
    async fn user(&self, quota: i64) -> String {
        create_user(&self.state.db,"test",quota).await.unwrap().1
    }
    async fn cleanup(self) {
        self.state.db.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE",self.schema)).execute(&self.admin).await.unwrap();
    }
}

async fn request(app: &Router, token: &str, method: &str, path: &str, body: Vec<u8>, json_body: bool) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(Request::builder().method(method).uri(path)
        .header("Authorization",format!("Bearer {token}"))
        .header("Content-Type",if json_body {"application/json"} else {"application/octet-stream"})
        .body(Body::from(body)).unwrap()).await.unwrap();
    let status = response.status();
    (status,to_bytes(response.into_body(),128 * 1024 * 1024).await.unwrap().to_vec())
}

async fn json_request(app: &Router, token: &str, method: &str, path: &str, body: Value) -> (StatusCode,Value) {
    let (status,bytes) = request(app,token,method,path,serde_json::to_vec(&body).unwrap(),true).await;
    (status,serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn mock_block(size: usize) -> (ChunkSpec,Vec<u8>) {
    let mut payload = OBJECT_HEADER.to_vec();
    let unique = Uuid::new_v4().into_bytes();
    payload.extend((0..size + 16).map(|index| unique[index % unique.len()]));
    (ChunkSpec {object_id:digest(&payload),size:size as i32},payload)
}

fn manifest(name: &str, chunks: &[ChunkSpec]) -> Value {
    json!({"name":name,"size":chunks.iter().map(|chunk|i64::from(chunk.size)).sum::<i64>(),
        "client_metadata":"opaque encrypted client manifest", "chunks":chunks})
}

fn upload_path(version: &Value, block: &ChunkSpec) -> String {
    format!("/v1/uploads/{}/blocks/{}",version["version_id"].as_str().unwrap(),block.object_id)
}

fn complete_path(version: &Value) -> String {format!("/v1/uploads/{}/complete",version["version_id"].as_str().unwrap())}

#[tokio::test]
#[ignore = "requires PostgreSQL and Redis"]
async fn full_upload_dedup_versions_permissions_and_deletion() {
    let h = Harness::new().await;
    let alice = h.user(10000).await;
    let bob = h.user(10000).await;
    let (a,pa) = mock_block(40);
    let (b,pb) = mock_block(60);
    let chunks = vec![a.clone(),a.clone(),b.clone()];
    let (status,first) = json_request(&h.app,&alice,"POST","/v1/files",manifest("file.bin",&chunks)).await;
    assert_eq!(status,StatusCode::CREATED);
    assert_eq!(first["missing"],json!([true,true,true]));
    assert_eq!(first["missing_chunks"].as_array().unwrap().len(),2);
    assert_eq!(first["upload_bytes"],100 + 2 * OBJECT_OVERHEAD as i64);
    let (status,error) = json_request(&h.app,&alice,"POST",&complete_path(&first),json!({})).await;
    assert_eq!(status,StatusCode::CONFLICT);
    assert_eq!(error["details"]["missing_object_ids"].as_array().unwrap().len(),2);
    assert_eq!(request(&h.app,&alice,"GET",&format!("/v1/blocks/{}",a.object_id),vec![],false).await.0,StatusCode::NOT_FOUND);
    assert_eq!(request(&h.app,&bob,"PUT",&upload_path(&first,&a),pa.clone(),false).await.0,StatusCode::NOT_FOUND);
    let mut bad = pa.clone(); *bad.last_mut().unwrap() ^= 1;
    assert_eq!(request(&h.app,&alice,"PUT",&upload_path(&first,&a),bad,false).await.0,StatusCode::BAD_REQUEST);
    assert_eq!(request(&h.app,&alice,"PUT",&upload_path(&first,&a),pa.clone(),false).await.0,StatusCode::CREATED);
    assert_eq!(request(&h.app,&alice,"PUT",&upload_path(&first,&a),pa.clone(),false).await.0,StatusCode::OK);
    assert_eq!(request(&h.app,&alice,"PUT",&upload_path(&first,&b),pb.clone(),false).await.0,StatusCode::CREATED);
    assert_eq!(h.state.cache.scoped("global").lookup(&chunks).await.len(),2);
    for _ in 0..2 { assert_eq!(json_request(&h.app,&alice,"POST",&complete_path(&first),json!({})).await.0,StatusCode::OK); }
    assert_eq!(request(&h.app,&alice,"PUT",&upload_path(&first,&a),pa.clone(),false).await.0,StatusCode::CONFLICT);
    let (status,download) = request(&h.app,&alice,"GET",&format!("/v1/blocks/{}",a.object_id),vec![],false).await;
    assert_eq!(status,StatusCode::OK); assert_eq!(download,pa);
    let (status,second) = json_request(&h.app,&alice,"POST","/v1/files",manifest("file.bin",&chunks)).await;
    assert_eq!(status,StatusCode::CREATED);
    assert_eq!(second["file_id"],first["file_id"]);
    assert_ne!(second["version_id"],first["version_id"]);
    assert_eq!(second["missing"],json!([false,false,false]));
    assert_eq!(second["upload_bytes"],0);
    assert_eq!(json_request(&h.app,&alice,"POST",&complete_path(&second),json!({})).await.0,StatusCode::OK);
    let version_path = format!("/v1/files/{}/versions/{}",first["file_id"].as_str().unwrap(),first["version_id"].as_str().unwrap());
    assert_eq!(json_request(&h.app,&bob,"GET",&version_path,json!({})).await.0,StatusCode::NOT_FOUND);
    let (status,version) = json_request(&h.app,&alice,"GET",&version_path,json!({})).await;
    assert_eq!(status,StatusCode::OK); assert_eq!(version["chunks"],json!(chunks));
    assert_eq!(json_request(&h.app,&bob,"GET",&format!("/v1/uploads/{}",first["version_id"].as_str().unwrap()),json!({})).await.0,StatusCode::NOT_FOUND);
    assert_eq!(request(&h.app,&bob,"GET",&format!("/v1/blocks/{}",a.object_id),vec![],false).await.0,StatusCode::NOT_FOUND);
    let (_,shared) = json_request(&h.app,&bob,"POST","/v1/files",manifest("shared.bin",&chunks)).await;
    assert_eq!(shared["upload_bytes"],0);
    assert_eq!(request(&h.app,&bob,"GET",&format!("/v1/blocks/{}",a.object_id),vec![],false).await.0,StatusCode::NOT_FOUND);
    assert_eq!(json_request(&h.app,&bob,"POST",&complete_path(&shared),json!({})).await.0,StatusCode::OK);
    let file_path = format!("/v1/files/{}",first["file_id"].as_str().unwrap());
    assert_eq!(request(&h.app,&bob,"DELETE",&file_path,vec![],false).await.0,StatusCode::NOT_FOUND);
    assert_eq!(request(&h.app,&alice,"DELETE",&file_path,vec![],false).await.0,StatusCode::NO_CONTENT);
    assert_eq!(json_request(&h.app,&alice,"GET","/v1/me",json!({})).await.1["used_bytes"],0);
    assert_eq!(request(&h.app,&alice,"GET",&format!("/v1/blocks/{}",a.object_id),vec![],false).await.0,StatusCode::NOT_FOUND);
    assert_eq!(request(&h.app,&bob,"GET",&format!("/v1/blocks/{}",a.object_id),vec![],false).await.1,pa);
    let references: i64 = sqlx::query_scalar("SELECT reference_count FROM block_references WHERE object_id=$1")
        .bind(&a.object_id).fetch_one(&h.state.db).await.unwrap();
    assert_eq!(references,2);
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL and Redis"]
async fn quota_concurrency_cancellation_empty_files_and_invalid_manifests() {
    let h = Harness::new().await;
    let token = h.user(40).await;
    let (a,_) = mock_block(30);
    let data = manifest("file",std::slice::from_ref(&a));
    let (left,right) = tokio::join!(json_request(&h.app,&token,"POST","/v1/files",data.clone()),
        json_request(&h.app,&token,"POST","/v1/files",data));
    let success = if left.0 == StatusCode::CREATED {left.1} else {right.1};
    assert!([left.0,right.0].contains(&StatusCode::CREATED));
    assert!([left.0,right.0].contains(&StatusCode::FORBIDDEN));
    assert_eq!(json_request(&h.app,&token,"GET","/v1/me",json!({})).await.1["used_bytes"],30);
    let cancel = format!("/v1/uploads/{}",success["version_id"].as_str().unwrap());
    assert_eq!(request(&h.app,&token,"DELETE",&cancel,vec![],false).await.0,StatusCode::NO_CONTENT);
    assert_eq!(json_request(&h.app,&token,"GET","/v1/me",json!({})).await.1["used_bytes"],0);
    let (_,empty) = json_request(&h.app,&token,"POST","/v1/files",manifest("empty",&[])).await;
    assert_eq!(empty["missing"],json!([]));
    assert_eq!(json_request(&h.app,&token,"POST",&complete_path(&empty),json!({})).await.0,StatusCode::OK);
    assert_eq!(request(&h.app,&token,"DELETE",&format!("/v1/uploads/{}",empty["version_id"].as_str().unwrap()),vec![],false).await.0,StatusCode::CONFLICT);
    for change in [json!({"size":-1}),json!({"size":2}),json!({"name":"../bad"}),json!({"chunks":[{"object_id":"invalid","size":1}],"size":1})] {
        let mut body = manifest("bad",&[]);
        for (key,value) in change.as_object().unwrap() {body[key] = value.clone();}
        assert_eq!(json_request(&h.app,&token,"POST","/v1/files",body).await.0,StatusCode::BAD_REQUEST);
    }
    assert_eq!(json_request(&h.app,&token,"GET","/v1/files?limit=0",json!({})).await.0,StatusCode::BAD_REQUEST);
    assert_eq!(json_request(&h.app,&"z".repeat(64),"GET","/v1/me",json!({})).await.0,StatusCode::UNAUTHORIZED);
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL and Redis"]
async fn folder_hierarchy_and_cross_account_foreign_keys() {
    let h = Harness::new().await;
    let alice = h.user(1000).await; let bob = h.user(1000).await;
    let (status,parent) = json_request(&h.app,&alice,"POST","/v1/directories",json!({"name":"docs"})).await;
    assert_eq!(status,StatusCode::CREATED);
    assert_eq!(json_request(&h.app,&alice,"POST","/v1/directories",json!({"name":"docs"})).await.0,StatusCode::CONFLICT);
    let nested = json!({"name":"nested","parent_id":parent["id"]});
    assert_eq!(json_request(&h.app,&bob,"POST","/v1/directories",nested.clone()).await.0,StatusCode::NOT_FOUND);
    assert_eq!(json_request(&h.app,&alice,"POST","/v1/directories",nested).await.0,StatusCode::CREATED);
    let mut file = manifest("file",&[]); file["parent_id"] = parent["id"].clone();
    assert_eq!(json_request(&h.app,&bob,"POST","/v1/files",file.clone()).await.0,StatusCode::NOT_FOUND);
    assert_eq!(json_request(&h.app,&alice,"POST","/v1/files",file).await.0,StatusCode::CREATED);
    assert_eq!(json_request(&h.app,&alice,"GET","/v1/files",json!({})).await.1["files"],json!([]));
    let listing = format!("/v1/files?parent_id={}",parent["id"].as_str().unwrap());
    assert_eq!(json_request(&h.app,&alice,"GET",&listing,json!({})).await.1["files"].as_array().unwrap().len(),1);
    let bob_id: Uuid = serde_json::from_value(json_request(&h.app,&bob,"GET","/v1/me",json!({})).await.1["id"].clone()).unwrap();
    let parent_id: Uuid = serde_json::from_value(parent["id"].clone()).unwrap();
    assert!(sqlx::query("INSERT INTO directories(id,user_id,parent_id,name) VALUES($1,$2,$3,'illegal')")
        .bind(Uuid::new_v4()).bind(bob_id).bind(parent_id).execute(&h.state.db).await.is_err());
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL and Redis"]
async fn concurrent_global_dedup_and_cache_failure_or_stale_hits() {
    let h = Harness::new().await;
    let alice = h.user(1000).await; let bob = h.user(1000).await;
    let (chunk,payload) = mock_block(100);
    let input = manifest("file",std::slice::from_ref(&chunk));
    let ((s1,v1),(s2,v2)) = tokio::join!(json_request(&h.app,&alice,"POST","/v1/files",input.clone()),
        json_request(&h.app,&bob,"POST","/v1/files",input));
    assert_eq!((s1,s2),(StatusCode::CREATED,StatusCode::CREATED));
    // A forged/stale cache entry cannot authorize a complete upload.
    h.state.cache.scoped("global").remember(std::slice::from_ref(&chunk)).await;
    assert_eq!(json_request(&h.app,&alice,"POST",&complete_path(&v1),json!({})).await.0,StatusCode::CONFLICT);
    let ((s1,_),(s2,_)) = tokio::join!(request(&h.app,&alice,"PUT",&upload_path(&v1,&chunk),payload.clone(),false),
        request(&h.app,&bob,"PUT",&upload_path(&v2,&chunk),payload.clone(),false));
    assert!([s1,s2].contains(&StatusCode::CREATED)); assert!([s1,s2].contains(&StatusCode::OK));
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM blocks WHERE available").fetch_one(&h.state.db).await.unwrap(),1);
    let mut fallback = h.state.clone();
    fallback.cache = BlockCache::new(&["redis://127.0.0.1:1".into()],false,"unreachable").unwrap();
    let fallback = router(fallback);
    let (status,requirements) = json_request(&fallback,&alice,"POST","/v1/blocks/check",json!({"chunks":[chunk]})).await;
    assert_eq!(status,StatusCode::OK); assert_eq!(requirements["missing"],json!([false]));
    assert_eq!(json_request(&fallback,&alice,"POST",&complete_path(&v1),json!({})).await.0,StatusCode::OK);
    assert_eq!(request(&fallback,&alice,"GET",&format!("/v1/blocks/{}",chunk.object_id),vec![],false).await.1,payload);
    let (other,_) = mock_block(20);
    assert_eq!(request(&h.app,&bob,"PUT",&upload_path(&v2,&other),vec![0;20 + OBJECT_OVERHEAD],false).await.0,StatusCode::NOT_FOUND);
    assert_eq!(request(&h.app,&bob,"PUT",&upload_path(&v2,&chunk),vec![0;1024 * 1024 + OBJECT_OVERHEAD + 1],false).await.0,StatusCode::PAYLOAD_TOO_LARGE);
    h.cleanup().await;
}
