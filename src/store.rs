use std::{
    fs::{self, File, OpenOptions},
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use uuid::Uuid;

use crate::model::{Email, Job};

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
    // Keep the cross-platform single-process lock alive for the lifetime of the pool.
    _lock: Arc<File>,
}

pub enum EnqueueResult {
    Accepted(Job),
    Full,
    Conflict,
}

#[derive(sqlx::FromRow)]
pub struct PendingJob {
    pub id: String,
    pub payload: String,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

impl Store {
    pub async fn open(directory: &Path) -> Result<Self> {
        fs::create_dir_all(directory).context("could not create DATA_DIR")?;
        let lock = private_file(&directory.join("service.lock"))?;
        lock.try_lock_exclusive()
            .context("DATA_DIR is already in use by another service process")?;
        let database = directory.join("courier.db");
        private_file(&database)?;
        let options = SqliteConnectOptions::new()
            .filename(&database)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(5))
            .pragma("secure_delete", "ON");
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL CHECK(status IN ('queued','sending','sent','failed','unknown')),
                payload TEXT,
                fingerprint BLOB NOT NULL,
                idempotency_key TEXT UNIQUE,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                error_code TEXT
            )"
        ).execute(&pool).await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS jobs_status_created ON jobs(status, created_at)")
            .execute(&pool)
            .await?;
        let store = Self {
            pool,
            _lock: Arc::new(lock),
        };
        // SMTP has no exactly-once guarantee. In-flight work at crash time is uncertain.
        store.recover().await?;
        Ok(store)
    }

    pub async fn recover(&self) -> Result<()> {
        sqlx::query("UPDATE jobs SET status='unknown', payload=NULL, error_code='interrupted', updated_at=? WHERE status='sending'")
            .bind(now()).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn healthy(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }

    pub async fn get(&self, id: &str) -> Result<Option<Job>> {
        Ok(
            sqlx::query_as(
                "SELECT id,status,created_at,updated_at,error_code FROM jobs WHERE id=?",
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await?,
        )
    }

    async fn replay(&self, key: &str, fingerprint: &[u8]) -> Result<Option<EnqueueResult>> {
        let existing: Option<(String, Vec<u8>)> =
            sqlx::query_as("SELECT id,fingerprint FROM jobs WHERE idempotency_key=?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        match existing {
            Some((id, hash)) if hash == fingerprint => {
                Ok(self.get(&id).await?.map(EnqueueResult::Accepted))
            }
            Some(_) => Ok(Some(EnqueueResult::Conflict)),
            None => Ok(None),
        }
    }

    pub async fn enqueue(
        &self,
        email: &Email,
        key: Option<&str>,
        max_pending: i64,
    ) -> Result<EnqueueResult> {
        let payload = serde_json::to_string(email)?;
        let fingerprint = Sha256::digest(payload.as_bytes()).to_vec();
        if let Some(key) = key {
            if let Some(result) = self.replay(key, &fingerprint).await? {
                return Ok(result);
            }
        }
        let id = Uuid::new_v4().to_string();
        let timestamp = now();
        // One SQL write statement makes the capacity check and insertion atomic.
        let result = sqlx::query(
            "INSERT INTO jobs(id,status,payload,fingerprint,idempotency_key,created_at,updated_at)
             SELECT ?, 'queued', ?, ?, ?, ?, ?
             WHERE (SELECT COUNT(*) FROM jobs WHERE status IN ('queued','sending')) < ?",
        )
        .bind(&id)
        .bind(payload)
        .bind(&fingerprint)
        .bind(key)
        .bind(timestamp)
        .bind(timestamp)
        .bind(max_pending)
        .execute(&self.pool)
        .await;
        match result {
            Ok(result) if result.rows_affected() == 1 => Ok(EnqueueResult::Accepted(Job {
                id,
                status: "queued".into(),
                created_at: timestamp,
                updated_at: timestamp,
                error_code: None,
            })),
            Ok(_) => {
                // Another concurrent request may have inserted this key while the queue filled.
                if let Some(key) = key {
                    if let Some(result) = self.replay(key, &fingerprint).await? {
                        return Ok(result);
                    }
                }
                Ok(EnqueueResult::Full)
            }
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() && key.is_some() => {
                self.replay(key.expect("checked"), &fingerprint)
                    .await?
                    .context("idempotency record disappeared")
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn claim(&self) -> Result<Option<PendingJob>> {
        Ok(sqlx::query_as(
            "UPDATE jobs SET status='sending', updated_at=?
             WHERE id=(SELECT id FROM jobs WHERE status='queued' ORDER BY created_at,rowid LIMIT 1)
             RETURNING id,payload",
        )
        .bind(now())
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn finish(&self, id: &str, status: &str, error_code: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE jobs SET status=?, error_code=?, payload=NULL, updated_at=? WHERE id=? AND status='sending'")
            .bind(status).bind(error_code).bind(now()).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn cleanup(&self, retention: Duration) -> Result<()> {
        sqlx::query(
            "DELETE FROM jobs WHERE status IN ('sent','failed','unknown') AND updated_at < ?",
        )
        .bind(now() - retention.as_secs() as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .context("could not open private data file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}
