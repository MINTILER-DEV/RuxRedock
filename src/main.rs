use std::{env, path::PathBuf};
use RuxRedock::{AppState, MIGRATOR, create_user, router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "RuxRedock=info,tower_http=info".into())
    ).init();
    let database_url = env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required")?;
    let arguments: Vec<String> = env::args().skip(1).collect();
    match arguments.first().map(String::as_str).unwrap_or("serve") {
        "migrate" => {
            let db = sqlx::PgPool::connect(&database_url).await?;
            MIGRATOR.run(&db).await?;
        }
        "create-user" => {
            let name = arguments.get(1).ok_or("usage: RuxRedock create-user NAME [QUOTA_BYTES]")?;
            RuxRedock::model::valid_name(name).map_err(|e| e.message)?;
            let quota = arguments.get(2).map(|q| q.parse::<i64>()).transpose()?.unwrap_or(107_374_182_400);
            if quota < 0 { return Err("quota must be nonnegative".into()); }
            let db = sqlx::PgPool::connect(&database_url).await?;
            MIGRATOR.run(&db).await?;
            let (id, token) = create_user(&db, name, quota).await?;
            println!("{}", serde_json::json!({"user_id": id, "token": token}));
        }
        "serve" => {
            let urls = env::var("REDIS_URLS").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
            let urls: Vec<String> = urls.split(',').map(|url| url.trim().to_owned()).collect();
            let cluster = match env::var("REDIS_CLUSTER").unwrap_or_else(|_| "false".into()).as_str() {
                "true" => true, "false" => false, _ => return Err("REDIS_CLUSTER must be true or false".into()),
            };
            let namespace = env::var("CACHE_NAMESPACE").unwrap_or_else(|_| "ruxredock:v1".into());
            let storage_dir = PathBuf::from(env::var("STORAGE_DIR").unwrap_or_else(|_| ".data".into()));
            let state = AppState::connect(&database_url, &urls, cluster, &namespace, &storage_dir).await?;
            let listener = tokio::net::TcpListener::bind(env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into())).await?;
            tracing::info!(address = %listener.local_addr()?, "API listening");
            axum::serve(listener, router(state)).with_graceful_shutdown(async {
                #[cfg(unix)] {
                    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
                    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
                }
                #[cfg(not(unix))] { let _ = tokio::signal::ctrl_c().await; }
            }).await?;
        }
        _ => return Err("usage: RuxRedock [serve | migrate | create-user NAME [QUOTA_BYTES]]".into()),
    }
    Ok(())
}
