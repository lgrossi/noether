use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

#[allow(dead_code)]
pub struct PostgresBackend {
    _marker: PhantomData<()>,
}

pub enum Backend {
    Sqlite(SqliteBackend),
    Postgres(PostgresBackend),
}

impl Backend {
    pub fn sqlite_from_url(db_url: String, conn: Arc<ConnMutex>) -> Self {
        Backend::Sqlite(SqliteBackend { conn, db_url })
    }

    pub fn sqlite_conn(&self) -> &Arc<ConnMutex> {
        match self {
            Backend::Sqlite(b) => &b.conn,
            Backend::Postgres(_) => panic!("Postgres backend not yet implemented"),
        }
    }

    pub fn db_url(&self) -> &str {
        match self {
            Backend::Sqlite(b) => &b.db_url,
            Backend::Postgres(_) => panic!("Postgres backend not yet implemented"),
        }
    }
}
