//! Positive-only Redis index. Failure/eviction is a PostgreSQL cache miss.
use crate::model::ChunkSpec;
use futures_util::{StreamExt, stream};
use redis::{
    AsyncCommands, aio::ConnectionManager, cluster::ClusterClient, cluster_async::ClusterConnection,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::timeout};

#[derive(Clone)]
enum Client {
    Single(redis::Client),
    Cluster(ClusterClient),
}

#[derive(Clone)]
enum Connection {
    Single(ConnectionManager),
    Cluster(ClusterConnection),
}

impl Connection {
    async fn get(&mut self, key: &str) -> redis::RedisResult<Option<i32>> {
        match self {
            Self::Single(c) => c.get(key).await,
            Self::Cluster(c) => c.get(key).await,
        }
    }
    async fn set(&mut self, key: &str, size: i32) -> redis::RedisResult<()> {
        match self {
            Self::Single(c) => c.set_ex(key, size, 3600).await,
            Self::Cluster(c) => c.set_ex(key, size, 3600).await,
        }
    }
    async fn ping(&mut self) -> redis::RedisResult<String> {
        match self {
            Self::Single(c) => redis::cmd("PING").query_async(c).await,
            Self::Cluster(c) => redis::cmd("PING").query_async(c).await,
        }
    }
}

#[derive(Clone)]
pub struct BlockCache {
    client: Client,
    connection: Arc<Mutex<Option<Connection>>>,
    namespace: Arc<str>,
}

impl BlockCache {
    pub fn new(urls: &[String], cluster: bool, namespace: &str) -> redis::RedisResult<Self> {
        let client = if cluster {
            Client::Cluster(ClusterClient::new(urls.to_vec())?)
        } else {
            Client::Single(redis::Client::open(
                urls.first()
                    .ok_or_else(|| {
                        redis::RedisError::from((
                            redis::ErrorKind::InvalidClientConfig,
                            "Redis URL is required",
                        ))
                    })?
                    .as_str(),
            )?)
        };
        Ok(Self {
            client,
            connection: Arc::new(Mutex::new(None)),
            namespace: namespace.into(),
        })
    }

    async fn connect(&self) -> Option<Connection> {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_ref() {
            return Some(connection.clone());
        }
        let result = timeout(Duration::from_millis(500), async {
            match &self.client {
                Client::Single(client) => client
                    .get_connection_manager()
                    .await
                    .map(Connection::Single),
                Client::Cluster(client) => {
                    client.get_async_connection().await.map(Connection::Cluster)
                }
            }
        })
        .await;
        match result {
            Ok(Ok(connection)) => {
                *guard = Some(connection.clone());
                Some(connection)
            }
            _ => None,
        }
    }

    fn key(&self, object_id: &str) -> String {
        format!("{}:block:{}", self.namespace, object_id)
    }

    pub async fn lookup(&self, chunks: &[ChunkSpec]) -> HashMap<String, i32> {
        let result = timeout(Duration::from_secs(1), async {
            let connection = self.connect().await?;
            // Per-key commands work across slots; arbitrary MGET would CROSSSLOT.
            let entries = stream::iter(chunks.to_vec().into_iter().map(|chunk| {
                let mut connection = connection.clone();
                let key = self.key(&chunk.object_id);
                async move {
                    let size = connection.get(&key).await.ok().flatten()?;
                    Some((chunk.object_id.clone(), size))
                }
            }))
            .buffer_unordered(64)
            .filter_map(|entry| async move { entry })
            .collect()
            .await;
            Some(entries)
        })
        .await;
        result.ok().flatten().unwrap_or_default()
    }

    pub async fn remember(&self, chunks: &[ChunkSpec]) {
        let _ = timeout(Duration::from_secs(1), async {
            let Some(connection) = self.connect().await else {
                return;
            };
            stream::iter(chunks.to_vec().into_iter().map(|chunk| {
                let mut connection = connection.clone();
                let key = self.key(&chunk.object_id);
                async move {
                    let _ = connection.set(&key, chunk.size).await;
                }
            }))
            .buffer_unordered(64)
            .collect::<Vec<_>>()
            .await;
        })
        .await;
    }

    pub async fn healthy(&self) -> bool {
        timeout(Duration::from_secs(1), async {
            if let Some(mut connection) = self.connect().await {
                connection.ping().await.is_ok()
            } else {
                false
            }
        })
        .await
        .unwrap_or(false)
    }

    pub fn scoped(&self, scope: &str) -> Self {
        Self {
            namespace: format!("{}:{scope}", self.namespace).into(),
            ..self.clone()
        }
    }
}
