#![allow(non_snake_case)]
mod api;
pub mod cache;
pub mod error;
pub mod model;
pub mod policy;
pub mod storage;

use crate::{
    cache::BlockCache,
    storage::{ObjectStore, digest},
};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{path::Path, time::Duration};
use uuid::Uuid;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub cache: BlockCache,
    pub storage: ObjectStore,
    pub policy: policy::UploadPolicy,
}

impl AppState {
    pub async fn connect(
        database_url: &str,
        redis_urls: &[String],
        cluster: bool,
        namespace: &str,
        storage_dir: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let db = PgPoolOptions::new()
            .max_connections(20)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("SET statement_timeout = '30s'")
                        .execute(&mut *conn)
                        .await?;
                    sqlx::query("SET lock_timeout = '10s'")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect(database_url)
            .await?;
        MIGRATOR.run(&db).await?;
        Ok(Self {
            db,
            cache: BlockCache::new(redis_urls, cluster, namespace)?,
            storage: ObjectStore::from_env(storage_dir)?,
            policy: policy::UploadPolicy::from_env()?,
        })
    }
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/me", get(api::me))
        .route(
            "/v1/directories",
            get(api::list_directories).post(api::create_directory),
        )
        .route(
            "/v1/directories/{directory_id}",
            axum::routing::patch(api::rename_directory).delete(api::delete_directory),
        )
        .route("/v1/files", get(api::list_files).post(api::ingest))
        .route(
            "/v1/files/{file_id}",
            get(api::get_file)
                .delete(api::delete_file)
                .patch(api::rename_file),
        )
        .route("/v1/files/{file_id}/versions", get(api::list_versions))
        .route(
            "/v1/files/{file_id}/versions/{version_id}",
            get(api::get_version),
        )
        .route(
            "/v1/uploads/{version_id}",
            get(api::upload_status).delete(api::cancel_upload),
        )
        .route("/v1/uploads/{version_id}/complete", post(api::complete))
        .route(
            "/v1/uploads/{version_id}/blocks/{object_id}",
            axum::routing::put(api::put_block).layer(DefaultBodyLimit::max(
                model::MAX_BLOCK_SIZE + model::OBJECT_OVERHEAD,
            )),
        )
        .route("/v1/blocks/check", post(api::check_blocks))
        .route("/v1/blocks/{object_id}", get(api::get_block))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api::authenticate,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            api::pace_response,
        ));
    Router::new()
        .merge(protected)
        .route("/health", get(api::health))
        .layer(DefaultBodyLimit::max(model::MAX_MANIFEST_BYTES))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .fallback_service(
            tower_http::services::ServeDir::new("frontend/dist")
                .append_index_html_on_directories(true),
        )
        .with_state(state)
}

/// Only the local administration CLI shows tokens; PostgreSQL holds their hashes.
pub async fn create_user(
    db: &PgPool,
    name: &str,
    quota: i64,
) -> Result<(Uuid, String), sqlx::Error> {
    let id = Uuid::new_v4();
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let mut tx = db.begin().await?;
    sqlx::query("INSERT INTO users (id,name,quota_bytes) VALUES ($1,$2,$3)")
        .bind(id)
        .bind(name)
        .bind(quota)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO api_tokens (token_hash,user_id) VALUES ($1,$2)")
        .bind(digest(token.as_bytes()))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((id, token))
}
