use std::any::Any;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::contract::{
    AuthorizeDecision, AuthorizeRequest, FinalizeReservation, Reservation, TraceEvent,
};
use crate::error::NoetError;
use crate::ledger::{AsyncPostgresLedger, BudgetLedger};
use crate::policy::PolicyFile;

#[derive(Clone)]
pub struct LedgerBackend {
    driver: Arc<dyn LedgerBackendDriver>,
}

impl LedgerBackend {
    pub fn in_memory() -> Self {
        Self {
            driver: Arc::new(InMemoryLedgerBackend),
        }
    }

    pub fn sqlite(path: PathBuf) -> Self {
        Self {
            driver: Arc::new(SqliteLedgerBackend { path }),
        }
    }

    pub fn postgres(database_url: String, ledger: AsyncPostgresLedger) -> Self {
        Self {
            driver: Arc::new(PostgresLedgerBackend {
                database_url,
                ledger,
            }),
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        self.driver.name()
    }

    pub(crate) fn postgres_async_finalize_failures(&self) -> Option<u64> {
        self.driver.postgres_async_finalize_failures()
    }

    pub(crate) async fn authorize_request(
        &self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        policy: Option<Arc<PolicyFile>>,
        request: AuthorizeRequest,
    ) -> Result<AuthorizeDecision, NoetError> {
        self.driver
            .authorize_request(sync_ledger, policy, request)
            .await
    }

    pub(crate) async fn finalize_reservation(
        &self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        reservation_id: String,
        payload: FinalizeReservation,
    ) -> Result<Reservation, NoetError> {
        self.driver
            .finalize_reservation(sync_ledger, reservation_id, payload)
            .await
    }

    pub(crate) async fn record_trace_event(
        &self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        event: TraceEvent,
    ) -> Result<(), NoetError> {
        self.driver.record_trace_event(sync_ledger, event).await
    }

    pub(crate) async fn read_ledger<T: Send + 'static>(
        &self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        read: impl FnOnce(&BudgetLedger) -> Result<T, NoetError> + Send + 'static,
    ) -> Result<T, NoetError> {
        let result = self
            .driver
            .read_ledger_boxed(
                sync_ledger,
                Box::new(move |ledger| {
                    read(ledger).map(|value| Box::new(value) as Box<dyn Any + Send>)
                }),
            )
            .await?;
        result.downcast::<T>().map(|value| *value).map_err(|_| {
            NoetError::InvalidConfig("ledger read returned unexpected result type".to_owned())
        })
    }
}

type LedgerBackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, NoetError>> + Send + 'a>>;
type LedgerReadBox =
    Box<dyn FnOnce(&BudgetLedger) -> Result<Box<dyn Any + Send>, NoetError> + Send + 'static>;

trait LedgerBackendDriver: Send + Sync {
    fn name(&self) -> &'static str;

    fn postgres_async_finalize_failures(&self) -> Option<u64> {
        None
    }

    fn authorize_request<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        policy: Option<Arc<PolicyFile>>,
        request: AuthorizeRequest,
    ) -> LedgerBackendFuture<'a, AuthorizeDecision>;

    fn finalize_reservation<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        reservation_id: String,
        payload: FinalizeReservation,
    ) -> LedgerBackendFuture<'a, Reservation>;

    fn record_trace_event<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        event: TraceEvent,
    ) -> LedgerBackendFuture<'a, ()>;

    fn read_ledger_boxed<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        read: LedgerReadBox,
    ) -> LedgerBackendFuture<'a, Box<dyn Any + Send>>;
}

struct InMemoryLedgerBackend;

struct SqliteLedgerBackend {
    path: PathBuf,
}

struct PostgresLedgerBackend {
    database_url: String,
    ledger: AsyncPostgresLedger,
}

impl LedgerBackendDriver for InMemoryLedgerBackend {
    fn name(&self) -> &'static str {
        "in_memory"
    }

    fn authorize_request<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        policy: Option<Arc<PolicyFile>>,
        request: AuthorizeRequest,
    ) -> LedgerBackendFuture<'a, AuthorizeDecision> {
        Box::pin(async move {
            spawn_sync_ledger_task(sync_ledger, move |ledger| {
                ledger.try_authorize(policy.as_deref(), &request)
            })
            .await
        })
    }

    fn finalize_reservation<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        reservation_id: String,
        payload: FinalizeReservation,
    ) -> LedgerBackendFuture<'a, Reservation> {
        Box::pin(async move {
            spawn_sync_ledger_task(sync_ledger, move |ledger| {
                ledger.finalize(&reservation_id, &payload)
            })
            .await
        })
    }

    fn record_trace_event<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        event: TraceEvent,
    ) -> LedgerBackendFuture<'a, ()> {
        Box::pin(async move {
            spawn_sync_ledger_task(sync_ledger, move |ledger| ledger.record_event(event)).await
        })
    }

    fn read_ledger_boxed<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        read: LedgerReadBox,
    ) -> LedgerBackendFuture<'a, Box<dyn Any + Send>> {
        Box::pin(async move {
            let ledger = sync_ledger.lock().await;
            read(&ledger)
        })
    }
}

impl LedgerBackendDriver for SqliteLedgerBackend {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn authorize_request<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        policy: Option<Arc<PolicyFile>>,
        request: AuthorizeRequest,
    ) -> LedgerBackendFuture<'a, AuthorizeDecision> {
        Box::pin(async move {
            spawn_sync_ledger_task(sync_ledger, move |ledger| {
                ledger.try_authorize(policy.as_deref(), &request)
            })
            .await
        })
    }

    fn finalize_reservation<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        reservation_id: String,
        payload: FinalizeReservation,
    ) -> LedgerBackendFuture<'a, Reservation> {
        Box::pin(async move {
            spawn_sync_ledger_task(sync_ledger, move |ledger| {
                ledger.finalize(&reservation_id, &payload)
            })
            .await
        })
    }

    fn record_trace_event<'a>(
        &'a self,
        sync_ledger: Arc<Mutex<BudgetLedger>>,
        event: TraceEvent,
    ) -> LedgerBackendFuture<'a, ()> {
        Box::pin(async move {
            spawn_sync_ledger_task(sync_ledger, move |ledger| ledger.record_event(event)).await
        })
    }

    fn read_ledger_boxed<'a>(
        &'a self,
        _sync_ledger: Arc<Mutex<BudgetLedger>>,
        read: LedgerReadBox,
    ) -> LedgerBackendFuture<'a, Box<dyn Any + Send>> {
        let path = self.path.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let ledger = BudgetLedger::open_sqlite(&path)?;
                read(&ledger)
            })
            .await
            .map_err(|error| {
                NoetError::InvalidConfig(format!("sqlite read task panicked: {error}"))
            })?
        })
    }
}

impl LedgerBackendDriver for PostgresLedgerBackend {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn postgres_async_finalize_failures(&self) -> Option<u64> {
        Some(self.ledger.async_finalize_failures())
    }

    fn authorize_request<'a>(
        &'a self,
        _sync_ledger: Arc<Mutex<BudgetLedger>>,
        policy: Option<Arc<PolicyFile>>,
        request: AuthorizeRequest,
    ) -> LedgerBackendFuture<'a, AuthorizeDecision> {
        Box::pin(async move { self.ledger.try_authorize(policy, request).await })
    }

    fn finalize_reservation<'a>(
        &'a self,
        _sync_ledger: Arc<Mutex<BudgetLedger>>,
        reservation_id: String,
        payload: FinalizeReservation,
    ) -> LedgerBackendFuture<'a, Reservation> {
        Box::pin(async move { self.ledger.finalize(reservation_id, payload).await })
    }

    fn record_trace_event<'a>(
        &'a self,
        _sync_ledger: Arc<Mutex<BudgetLedger>>,
        event: TraceEvent,
    ) -> LedgerBackendFuture<'a, ()> {
        Box::pin(async move { self.ledger.record_event(event).await })
    }

    fn read_ledger_boxed<'a>(
        &'a self,
        _sync_ledger: Arc<Mutex<BudgetLedger>>,
        read: LedgerReadBox,
    ) -> LedgerBackendFuture<'a, Box<dyn Any + Send>> {
        let database_url = self.database_url.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let ledger = BudgetLedger::open_postgres(&database_url)?;
                read(&ledger)
            })
            .await
            .map_err(|error| {
                NoetError::InvalidConfig(format!("postgres read task panicked: {error}"))
            })?
        })
    }
}

async fn spawn_sync_ledger_task<T: Send + 'static>(
    sync_ledger: Arc<Mutex<BudgetLedger>>,
    task: impl FnOnce(&mut BudgetLedger) -> Result<T, NoetError> + Send + 'static,
) -> Result<T, NoetError> {
    tokio::task::spawn_blocking(move || task(&mut sync_ledger.blocking_lock()))
        .await
        .map_err(|error| NoetError::InvalidConfig(format!("ledger task panicked: {error}")))?
}
