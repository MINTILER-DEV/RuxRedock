use crate::{
    AppState,
    error::{ApiError, Result},
    model::*,
    storage::digest,
};
use axum::{
    Extension, Json,
    body::Bytes,
    extract::{Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::collections::{BTreeMap, HashMap};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct User(pub Uuid);

pub async fn pace_response(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let deadline = tokio::time::Instant::now() + state.policy.minimum_response;
    let response = next.run(request).await;
    tokio::time::sleep_until(deadline).await;
    response
}

pub async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|token| token.len() == 64)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "valid bearer token required",
            )
        })?;
    let user: Option<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM api_tokens WHERE token_hash=$1")
            .bind(digest(token.as_bytes()))
            .fetch_optional(&state.db)
            .await?;
    let user = user.ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "valid bearer token required",
        )
    })?;
    request.extensions_mut().insert(User(user));
    Ok(next.run(request).await)
}

pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let database = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();
    let redis = state.cache.healthy().await;
    (
        if database {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(
            json!({"database": database, "redis": redis, "status": if database && redis {"ok"} else {"degraded"}}),
        ),
    )
}

pub async fn me(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
) -> Result<Json<Value>> {
    let row = sqlx::query("SELECT id,name,quota_bytes,used_bytes FROM users WHERE id=$1")
        .bind(user.0)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(
        json!({"id": row.get::<Uuid,_>("id"), "name": row.get::<String,_>("name"),
        "quota_bytes": row.get::<i64,_>("quota_bytes"), "used_bytes": row.get::<i64,_>("used_bytes")}),
    ))
}

async fn check_parent(
    tx: &mut Transaction<'_, Postgres>,
    user: Uuid,
    parent: Option<Uuid>,
) -> Result<()> {
    if let Some(parent) = parent {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM directories WHERE id=$1 AND user_id=$2)",
        )
        .bind(parent)
        .bind(user)
        .fetch_one(&mut **tx)
        .await?;
        if !exists {
            return Err(ApiError::not_found());
        }
    }
    Ok(())
}

pub async fn create_directory(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Json(input): Json<DirectoryInput>,
) -> Result<(StatusCode, Json<Value>)> {
    valid_name(&input.name)?;
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user.0)
        .execute(&mut *tx)
        .await?;
    check_parent(&mut tx, user.0, input.parent_id).await?;
    let collision:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM files WHERE user_id=$1 AND parent_id IS NOT DISTINCT FROM $2 AND name=$3)")
        .bind(user.0).bind(input.parent_id).bind(&input.name).fetch_one(&mut *tx).await?;
    if collision {
        return Err(ApiError::conflict("a file with this name already exists"));
    }
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO directories(id,user_id,parent_id,name) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING RETURNING id")
        .bind(Uuid::new_v4()).bind(user.0).bind(input.parent_id).bind(&input.name)
        .fetch_optional(&mut *tx).await?;
    let id = id.ok_or_else(|| ApiError::conflict("directory name already exists"))?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "name": input.name, "parent_id": input.parent_id})),
    ))
}

pub async fn list_directories(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Query(query): Query<Listing>,
) -> Result<Json<Value>> {
    let (limit, offset) = query.pagination()?;
    let rows = sqlx::query("SELECT id,name,parent_id FROM directories WHERE user_id=$1 AND parent_id IS NOT DISTINCT FROM $2 ORDER BY name,id LIMIT $3 OFFSET $4")
        .bind(user.0).bind(query.parent_id).bind(limit).bind(offset).fetch_all(&state.db).await?;
    Ok(Json(
        json!({"directories": rows.iter().map(|row| json!({"id": row.get::<Uuid,_>("id"),
        "name": row.get::<String,_>("name"), "parent_id": row.get::<Option<Uuid>,_>("parent_id")})).collect::<Vec<_>>()}),
    ))
}

fn unique_chunks(chunks: &[ChunkSpec]) -> Vec<ChunkSpec> {
    chunks
        .iter()
        .map(|chunk| (chunk.object_id.clone(), chunk.size))
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .map(|(object_id, size)| ChunkSpec { object_id, size })
        .collect()
}

async fn available(
    state: &AppState,
    user: Uuid,
    chunks: &[ChunkSpec],
) -> Result<HashMap<String, i32>> {
    let unique = unique_chunks(chunks);
    let scope = if state.policy.global {
        "global".to_string()
    } else {
        user.to_string()
    };
    let cache = state.cache.scoped(&scope);
    let mut found = cache.lookup(&unique).await;
    // Reject invalid cache sizes as misses too.
    found.retain(|id, size| {
        unique
            .binary_search_by(|chunk| chunk.object_id.cmp(id))
            .is_ok_and(|index| unique[index].size == *size)
    });
    let misses: Vec<_> = unique
        .iter()
        .filter(|chunk| !found.contains_key(&chunk.object_id))
        .map(|chunk| chunk.object_id.clone())
        .collect();
    if !misses.is_empty() {
        let rows = sqlx::query("SELECT object_id,size FROM blocks b WHERE available AND object_id=ANY($1) AND ($2 OR EXISTS(SELECT 1 FROM block_claims c WHERE c.object_id=b.object_id AND c.user_id=$3))")
            .bind(&misses).bind(state.policy.global).bind(user).fetch_all(&state.db).await?;
        let mut remembered = Vec::new();
        for row in rows {
            let id: String = row.get("object_id");
            let size: i32 = row.get("size");
            let index = unique
                .binary_search_by(|chunk| chunk.object_id.cmp(&id))
                .unwrap();
            if unique[index].size != size {
                return Err(ApiError::conflict(
                    "object identifier has a different stored size",
                ));
            }
            found.insert(id.clone(), size);
            remembered.push(ChunkSpec {
                object_id: id,
                size,
            });
        }
        cache.remember(&remembered).await;
    }
    Ok(found)
}

async fn requirements(state: &AppState, user: Uuid, chunks: &[ChunkSpec]) -> Result<Value> {
    let found = available(state, user, chunks).await?;
    let missing: Vec<bool> = chunks
        .iter()
        .map(|chunk| !found.contains_key(&chunk.object_id))
        .collect();
    let unique_missing: Vec<_> = unique_chunks(chunks)
        .into_iter()
        .filter(|chunk| !found.contains_key(&chunk.object_id))
        .collect();
    let bytes: i64 = unique_missing
        .iter()
        .map(|chunk| i64::from(chunk.size) + OBJECT_OVERHEAD as i64)
        .sum();
    Ok(json!({"missing": missing, "missing_chunks": unique_missing, "upload_bytes": bytes}))
}

pub async fn check_blocks(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Json(input): Json<Lookup>,
) -> Result<Json<Value>> {
    if input.chunks.len() > MAX_CHUNKS {
        return Err(ApiError::bad("too many chunks"));
    }
    let size = input.chunks.iter().map(|chunk| i64::from(chunk.size)).sum();
    Manifest {
        name: "lookup".into(),
        parent_id: None,
        size,
        client_metadata: "".into(),
        chunks: input.chunks.clone(),
    }
    .validate()?;
    Ok(Json(requirements(&state, user.0, &input.chunks).await?))
}

pub async fn ingest(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Json(manifest): Json<Manifest>,
) -> Result<(StatusCode, Json<Value>)> {
    manifest.validate()?;
    let unique = unique_chunks(&manifest.chunks);
    let ids: Vec<_> = unique.iter().map(|chunk| chunk.object_id.clone()).collect();
    let sizes: Vec<_> = unique.iter().map(|chunk| chunk.size).collect();
    let version_id = Uuid::new_v4();
    let mut tx = state.db.begin().await?;
    // A per-user lock makes quota reservation and version creation atomic.
    let row = sqlx::query("SELECT quota_bytes,used_bytes FROM users WHERE id=$1 FOR UPDATE")
        .bind(user.0)
        .fetch_one(&mut *tx)
        .await?;
    if manifest.size > row.get::<i64, _>("quota_bytes") - row.get::<i64, _>("used_bytes") {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "quota_exceeded",
            "logical storage quota exceeded",
        ));
    }
    check_parent(&mut tx, user.0, manifest.parent_id).await?;
    let collision:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM directories WHERE user_id=$1 AND parent_id IS NOT DISTINCT FROM $2 AND name=$3)")
        .bind(user.0).bind(manifest.parent_id).bind(&manifest.name).fetch_one(&mut *tx).await?;
    if collision {
        return Err(ApiError::conflict("a folder with this name already exists"));
    }
    // Sorted IDs ensure competing global-index inserts lock in the same order.
    sqlx::query("INSERT INTO blocks(object_id,size) SELECT * FROM unnest($1::text[],$2::int[]) ORDER BY 1 ON CONFLICT(object_id) DO NOTHING")
        .bind(&ids).bind(&sizes).execute(&mut *tx).await?;
    let rows = sqlx::query("SELECT object_id,size FROM blocks b WHERE object_id=ANY($1) AND available AND ($2 OR EXISTS(SELECT 1 FROM block_claims c WHERE c.object_id=b.object_id AND c.user_id=$3))")
        .bind(&ids).bind(state.policy.global).bind(user.0)
        .fetch_all(&mut *tx)
        .await?;
    for row in rows {
        let id: String = row.get("object_id");
        let index = ids.binary_search(&id).unwrap();
        if row.get::<i32, _>("size") != sizes[index] {
            return Err(ApiError::conflict(
                "object identifier has a different stored size",
            ));
        }
    }
    let file_id: Uuid = sqlx::query_scalar("INSERT INTO files(id,user_id,parent_id,name) VALUES($1,$2,$3,$4) ON CONFLICT(user_id,parent_id,name) DO UPDATE SET name=EXCLUDED.name RETURNING id")
        .bind(Uuid::new_v4()).bind(user.0).bind(manifest.parent_id).bind(&manifest.name)
        .fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO file_versions(id,file_id,size,client_metadata) VALUES($1,$2,$3,$4)")
        .bind(version_id)
        .bind(file_id)
        .bind(manifest.size)
        .bind(&manifest.client_metadata)
        .execute(&mut *tx)
        .await?;
    let ordinals: Vec<i32> = (0..manifest.chunks.len() as i32).collect();
    let ordered_ids: Vec<_> = manifest
        .chunks
        .iter()
        .map(|chunk| chunk.object_id.clone())
        .collect();
    let declared_sizes: Vec<i32> = manifest.chunks.iter().map(|chunk| chunk.size).collect();
    sqlx::query("INSERT INTO file_chunks(version_id,ordinal,object_id,size) SELECT $1,* FROM unnest($2::int[],$3::text[],$4::int[])")
        .bind(version_id).bind(&ordinals).bind(&ordered_ids).bind(&declared_sizes).execute(&mut *tx).await?;
    sqlx::query("UPDATE users SET used_bytes=used_bytes+$2 WHERE id=$1")
        .bind(user.0)
        .bind(manifest.size)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let mut response = requirements(&state, user.0, &manifest.chunks).await?;
    response["file_id"] = json!(file_id);
    response["version_id"] = json!(version_id);
    response["status"] = json!("pending");
    Ok((StatusCode::CREATED, Json(response)))
}

async fn lock_version(
    tx: &mut Transaction<'_, Postgres>,
    user: Uuid,
    version: Uuid,
) -> Result<sqlx::postgres::PgRow> {
    sqlx::query("SELECT v.id,v.file_id,v.size,v.status,extract(epoch FROM (clock_timestamp()-v.created_at))::double precision AS elapsed_seconds FROM file_versions v JOIN files f ON f.id=v.file_id WHERE v.id=$1 AND f.user_id=$2 FOR UPDATE OF v")
        .bind(version).bind(user).fetch_optional(&mut **tx).await?.ok_or_else(ApiError::not_found)
}

async fn version_chunks(state: &AppState, version: Uuid) -> Result<Vec<ChunkSpec>> {
    let rows =
        sqlx::query("SELECT object_id,size FROM file_chunks WHERE version_id=$1 ORDER BY ordinal")
            .bind(version)
            .fetch_all(&state.db)
            .await?;
    Ok(rows
        .iter()
        .map(|row| ChunkSpec {
            object_id: row.get("object_id"),
            size: row.get("size"),
        })
        .collect())
}

pub async fn upload_status(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(version_id): Path<Uuid>,
) -> Result<Json<Value>> {
    let row = sqlx::query("SELECT v.file_id,v.status FROM file_versions v JOIN files f ON f.id=v.file_id WHERE v.id=$1 AND f.user_id=$2")
        .bind(version_id).bind(user.0).fetch_optional(&state.db).await?.ok_or_else(ApiError::not_found)?;
    let chunks = version_chunks(&state, version_id).await?;
    let mut response = requirements(&state, user.0, &chunks).await?;
    response["file_id"] = json!(row.get::<Uuid, _>("file_id"));
    response["version_id"] = json!(version_id);
    response["status"] = json!(row.get::<String, _>("status"));
    Ok(Json(response))
}

pub async fn put_block(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path((version_id, object_id)): Path<(Uuid, String)>,
    payload: Bytes,
) -> Result<(StatusCode, Json<Value>)> {
    valid_hash(&object_id)?;
    let mut tx = state.db.begin().await?;
    let version = lock_version(&mut tx, user.0, version_id).await?;
    if version.get::<String, _>("status") != "pending" {
        return Err(ApiError::conflict("upload already completed"));
    }
    let size: Option<i32> = sqlx::query_scalar(
        "SELECT DISTINCT size FROM file_chunks WHERE object_id=$1 AND version_id=$2",
    )
    .bind(&object_id)
    .bind(version_id)
    .fetch_optional(&mut *tx)
    .await?;
    let size = size.ok_or_else(ApiError::not_found)?;
    if payload.len() != size as usize + OBJECT_OVERHEAD {
        return Err(ApiError::bad("encrypted payload size mismatch"));
    }
    if !payload.starts_with(OBJECT_HEADER) {
        return Err(ApiError::bad("unsupported ciphertext format"));
    }
    if digest(&payload) != object_id {
        return Err(ApiError::bad("encrypted payload hash mismatch"));
    }
    let created = state
        .storage
        .put(object_id.clone(), payload)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query(
        "UPDATE blocks SET size=$2,available=true,stored_at=COALESCE(stored_at,now()) WHERE object_id=$1",
    )
    .bind(&object_id)
    .bind(size)
    .execute(&mut *tx)
    .await?;
    let claimed = sqlx::query(
        "INSERT INTO block_claims(user_id,object_id) VALUES($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(user.0)
    .bind(&object_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    tx.commit().await?;
    let scope = if state.policy.global {
        "global".to_string()
    } else {
        user.0.to_string()
    };
    state
        .cache
        .scoped(&scope)
        .remember(&[ChunkSpec {
            object_id: object_id.clone(),
            size,
        }])
        .await;
    let created = if state.policy.global {
        created
    } else {
        claimed
    };
    Ok((
        if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(json!({"object_id":object_id,"created":created})),
    ))
}

pub async fn complete(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(version_id): Path<Uuid>,
) -> Result<Json<Value>> {
    let mut tx = state.db.begin().await?;
    let version = lock_version(&mut tx, user.0, version_id).await?;
    // Never trust cache hits to grant a readable version.
    let missing: Vec<String> = sqlx::query_scalar("SELECT DISTINCT c.object_id FROM file_chunks c JOIN blocks b USING(object_id) WHERE c.version_id=$1 AND (NOT b.available OR b.size<>c.size OR (NOT $2 AND NOT EXISTS(SELECT 1 FROM block_claims p WHERE p.object_id=b.object_id AND p.user_id=$3))) ORDER BY c.object_id")
        .bind(version_id).bind(state.policy.global).bind(user.0).fetch_all(&mut *tx).await?;
    if !missing.is_empty() {
        let mut error = ApiError::new(
            StatusCode::CONFLICT,
            "missing_blocks",
            "upload is incomplete",
        );
        error.details = Some(json!({"missing_object_ids":missing}));
        return Err(error);
    }
    tx.commit().await?;
    let target = state.policy.minimum_response.as_secs_f64()
        + version.get::<i64, _>("size") as f64 / state.policy.bytes_per_second as f64;
    let remaining = (target - version.get::<f64, _>("elapsed_seconds")).max(0.0);
    // Async pacing uses no worker threads and applies equally to reused/new blocks.
    tokio::time::sleep(std::time::Duration::from_secs_f64(remaining)).await;
    // Do not expose ready metadata before the same timing floor. Release the
    // database connection while waiting, then revalidate atomically on commit.
    let mut tx = state.db.begin().await?;
    lock_version(&mut tx, user.0, version_id).await?;
    let missing:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM file_chunks c JOIN blocks b USING(object_id) WHERE c.version_id=$1 AND (NOT b.available OR b.size<>c.size OR (NOT $2 AND NOT EXISTS(SELECT 1 FROM block_claims p WHERE p.object_id=b.object_id AND p.user_id=$3))))")
        .bind(version_id).bind(state.policy.global).bind(user.0).fetch_one(&mut *tx).await?;
    if missing {
        return Err(ApiError::conflict("upload is incomplete"));
    }
    sqlx::query("UPDATE file_versions SET status='ready',completed_at=COALESCE(completed_at,now()) WHERE id=$1")
        .bind(version_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"file_id":version.get::<Uuid,_>("file_id"), "version_id":version_id,"status":"ready"}),
    ))
}

pub async fn get_block(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(object_id): Path<String>,
) -> Result<Response> {
    valid_hash(&object_id)?;
    let size: Option<i32> = sqlx::query_scalar("SELECT b.size FROM blocks b WHERE b.object_id=$1 AND b.available AND EXISTS(SELECT 1 FROM file_chunks c JOIN file_versions v ON v.id=c.version_id JOIN files f ON f.id=v.file_id WHERE c.object_id=b.object_id AND v.status='ready' AND f.user_id=$2)")
        .bind(&object_id).bind(user.0).fetch_optional(&state.db).await?;
    let size = size.ok_or_else(ApiError::not_found)?;
    let payload = state
        .storage
        .get(object_id.clone(), size as usize + OBJECT_OVERHEAD)
        .await
        .map_err(ApiError::internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        payload,
    )
        .into_response())
}

pub async fn list_files(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Query(query): Query<Listing>,
) -> Result<Json<Value>> {
    let (limit, offset) = query.pagination()?;
    let rows = sqlx::query("SELECT f.id,f.name,f.parent_id,COALESCE(v.size,0) AS size,COALESCE(v.created_at,f.created_at)::text AS modified,COALESCE(v.status,'pending') AS status,(SELECT count(*) FROM file_versions WHERE file_id=f.id) AS version_count FROM files f LEFT JOIN LATERAL(SELECT size,created_at,status FROM file_versions WHERE file_id=f.id ORDER BY created_at DESC,id DESC LIMIT 1)v ON true WHERE user_id=$1 AND parent_id IS NOT DISTINCT FROM $2 ORDER BY name,id LIMIT $3 OFFSET $4")
        .bind(user.0).bind(query.parent_id).bind(limit).bind(offset).fetch_all(&state.db).await?;
    Ok(Json(
        json!({"files":rows.iter().map(|row|json!({"id":row.get::<Uuid,_>("id"),
        "name":row.get::<String,_>("name"),"parent_id":row.get::<Option<Uuid>,_>("parent_id"),
        "size":row.get::<i64,_>("size"),"modified":row.get::<String,_>("modified"),
        "status":row.get::<String,_>("status"),"version_count":row.get::<i64,_>("version_count")})).collect::<Vec<_>>()}),
    ))
}

pub async fn get_file(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(file_id): Path<Uuid>,
) -> Result<Json<Value>> {
    let row = sqlx::query("SELECT id,name,parent_id FROM files WHERE id=$1 AND user_id=$2")
        .bind(file_id)
        .bind(user.0)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(
        json!({"id":file_id,"name":row.get::<String,_>("name"),"parent_id":row.get::<Option<Uuid>,_>("parent_id")}),
    ))
}

pub async fn list_versions(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(file_id): Path<Uuid>,
    Query(query): Query<Listing>,
) -> Result<Json<Value>> {
    let (limit, offset) = query.pagination()?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM files WHERE id=$1 AND user_id=$2)")
            .bind(file_id)
            .bind(user.0)
            .fetch_one(&state.db)
            .await?;
    if !exists {
        return Err(ApiError::not_found());
    }
    let rows = sqlx::query("SELECT id,size,status,created_at::text,completed_at::text FROM file_versions WHERE file_id=$1 ORDER BY created_at DESC,id DESC LIMIT $2 OFFSET $3")
        .bind(file_id).bind(limit).bind(offset).fetch_all(&state.db).await?;
    Ok(Json(
        json!({"versions":rows.iter().map(|row|json!({"id":row.get::<Uuid,_>("id"),
        "size":row.get::<i64,_>("size"),"status":row.get::<String,_>("status"),
        "created_at":row.get::<String,_>("created_at"),"completed_at":row.get::<Option<String>,_>("completed_at")})).collect::<Vec<_>>()}),
    ))
}

pub async fn get_version(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path((file_id, version_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>> {
    let row = sqlx::query("SELECT v.size,v.status,v.client_metadata FROM file_versions v JOIN files f ON f.id=v.file_id WHERE v.id=$1 AND v.file_id=$2 AND f.user_id=$3")
        .bind(version_id).bind(file_id).bind(user.0).fetch_optional(&state.db).await?.ok_or_else(ApiError::not_found)?;
    Ok(Json(
        json!({"file_id":file_id,"version_id":version_id,"size":row.get::<i64,_>("size"),
        "status":row.get::<String,_>("status"),"client_metadata":row.get::<String,_>("client_metadata"),
        "chunks":version_chunks(&state,version_id).await?}),
    ))
}

pub async fn delete_file(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(file_id): Path<Uuid>,
) -> Result<StatusCode> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user.0)
        .execute(&mut *tx)
        .await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM files WHERE id=$1 AND user_id=$2)")
            .bind(file_id)
            .bind(user.0)
            .fetch_one(&mut *tx)
            .await?;
    if !exists {
        return Err(ApiError::not_found());
    }
    let size: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(size),0)::bigint FROM file_versions WHERE file_id=$1",
    )
    .bind(file_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM files WHERE id=$1")
        .bind(file_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE users SET used_bytes=used_bytes-$2 WHERE id=$1")
        .bind(user.0)
        .bind(size)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn cancel_upload(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(version_id): Path<Uuid>,
) -> Result<StatusCode> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user.0)
        .execute(&mut *tx)
        .await?;
    let version = lock_version(&mut tx, user.0, version_id).await?;
    if version.get::<String, _>("status") != "pending" {
        return Err(ApiError::conflict("completed versions cannot be cancelled"));
    }
    let file_id: Uuid = version.get("file_id");
    sqlx::query("DELETE FROM file_versions WHERE id=$1")
        .bind(version_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM files WHERE id=$1 AND NOT EXISTS(SELECT 1 FROM file_versions WHERE file_id=$1)")
        .bind(file_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE users SET used_bytes=used_bytes-$2 WHERE id=$1")
        .bind(user.0)
        .bind(version.get::<i64, _>("size"))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rename_resource(
    state: &AppState,
    user: Uuid,
    id: Uuid,
    input: RenameInput,
    folder: bool,
) -> Result<Json<Value>> {
    valid_name(&input.name)?;
    let table = if folder { "directories" } else { "files" };
    let other = if folder { "files" } else { "directories" };
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query(&format!(
        "SELECT parent_id FROM {table} WHERE id=$1 AND user_id=$2"
    ))
    .bind(id)
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let parent: Option<Uuid> = row.get("parent_id");
    let collision:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM {other} WHERE user_id=$1 AND parent_id IS NOT DISTINCT FROM $2 AND name=$3)"))
        .bind(user).bind(parent).bind(&input.name).fetch_one(&mut *tx).await?;
    if collision {
        return Err(ApiError::conflict("name already in use"));
    }
    let result = sqlx::query(&format!(
        "UPDATE {table} SET name=$3 WHERE id=$1 AND user_id=$2"
    ))
    .bind(id)
    .bind(user)
    .bind(&input.name)
    .execute(&mut *tx)
    .await;
    match result {
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23505") => {
            return Err(ApiError::conflict("name already in use"));
        }
        Err(error) => return Err(error.into()),
        _ => {}
    }
    tx.commit().await?;
    Ok(Json(json!({"id":id,"name":input.name})))
}
pub async fn rename_file(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(id): Path<Uuid>,
    Json(input): Json<RenameInput>,
) -> Result<Json<Value>> {
    rename_resource(&state, user.0, id, input, false).await
}
pub async fn rename_directory(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(id): Path<Uuid>,
    Json(input): Json<RenameInput>,
) -> Result<Json<Value>> {
    rename_resource(&state, user.0, id, input, true).await
}
pub async fn delete_directory(
    State(state): State<AppState>,
    Extension(user): Extension<User>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user.0)
        .execute(&mut *tx)
        .await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM directories WHERE id=$1 AND user_id=$2)")
            .bind(id)
            .bind(user.0)
            .fetch_one(&mut *tx)
            .await?;
    if !exists {
        return Err(ApiError::not_found());
    }
    let children:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM files WHERE parent_id=$1) OR EXISTS(SELECT 1 FROM directories WHERE parent_id=$1)")
        .bind(id).fetch_one(&mut *tx).await?;
    if children {
        return Err(ApiError::conflict("empty the folder before deleting it"));
    }
    sqlx::query("DELETE FROM directories WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
