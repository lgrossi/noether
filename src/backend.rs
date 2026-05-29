use std::marker::PhantomData;
use std::sync::Arc;

use crate::ledger::ConnMutex;

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
