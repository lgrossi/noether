use std::path::{Path, PathBuf};
use std::sync::Arc;

use deadpool_postgres::Pool;

use crate::error::NoetError;
use crate::ledger::ConnMutex;

/// Convert a filesystem path to a sqlite:// URL.
/// Resolves the path to absolute using the current working directory.
pub fn path_to_sqlite_url(p: &Path) -> String {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    };
    format!("sqlite://{}", abs.display())
}

/// Extract the filesystem path from a sqlite:// URL.
/// Returns None if the URL is empty or does not start with "sqlite://".
pub fn sqlite_url_to_path(url: &str) -> Option<PathBuf> {
    let tail = url.strip_prefix("sqlite://")?;
    if tail.is_empty() {
        return None;
    }
    Some(PathBuf::from(tail))
}

pub struct SqliteBackend {
    pub conn: Arc<ConnMutex>,
    pub db_url: String,
}

pub struct PostgresBackend {
    pub pool: Arc<Pool>,
    pub db_url: String,
}

pub enum Backend {
    Sqlite(SqliteBackend),
    Postgres(PostgresBackend),
}

impl Backend {
    pub fn sqlite_from_url(db_url: String, conn: Arc<ConnMutex>) -> Self {
        Backend::Sqlite(SqliteBackend { conn, db_url })
    }

    pub fn postgres_from_url(db_url: String) -> Result<Self, NoetError> {
        let pg_config: tokio_postgres::Config = db_url.parse().map_err(|e: tokio_postgres::Error| {
            NoetError::InvalidConfig(format!("invalid postgres URL: {e}"))
        })?;
        let mgr_config = deadpool_postgres::ManagerConfig {
            recycling_method: deadpool_postgres::RecyclingMethod::Fast,
        };
        let mgr = deadpool_postgres::Manager::from_config(
            pg_config,
            tokio_postgres::NoTls,
            mgr_config,
        );
        let pool = deadpool_postgres::Pool::builder(mgr)
            .max_size(4)
            .build()
            .map_err(|e| NoetError::InvalidConfig(format!("failed to build postgres pool: {e}")))?;
        Ok(Backend::Postgres(PostgresBackend {
            pool: Arc::new(pool),
            db_url,
        }))
    }

    pub fn sqlite_conn(&self) -> &Arc<ConnMutex> {
        match self {
            Backend::Sqlite(b) => &b.conn,
            Backend::Postgres(_) => panic!("Postgres backend not yet implemented"),
        }
    }

    pub fn postgres_pool(&self) -> &Arc<Pool> {
        match self {
            Backend::Sqlite(_) => panic!("postgres_pool called on Sqlite backend"),
            Backend::Postgres(b) => &b.pool,
        }
    }

    pub fn db_url(&self) -> &str {
        match self {
            Backend::Sqlite(b) => &b.db_url,
            Backend::Postgres(b) => &b.db_url,
        }
    }
}

/// Return the URL scheme ("sqlite" or "postgres") without the trailing "://",
/// or None if the URL contains no "://" separator.
pub fn url_scheme(url: &str) -> Option<&str> {
    let end = url.find("://")?;
    Some(&url[..end])
}
