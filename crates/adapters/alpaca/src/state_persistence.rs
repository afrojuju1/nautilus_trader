//! Async strategy-state persistence boundary for Alpaca live strategies.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use nautilus_infrastructure::sql::operational::{
    OperationalRepository, StrategyStateMutation, heartbeat_runtime_lease,
    persist_strategy_state_mutation,
};

use crate::{
    candidate_ledger_persistence::CandidateLedgerPersistenceHandle,
    runtime::{StrategyState, emit_operator_event},
};
use serde_json::json;

const STATE_PERSISTENCE_QUEUE_CAPACITY: usize = 64;
const STATE_PERSISTENCE_FLUSH_TIMEOUT_SECS: u64 = 5;

#[derive(Clone, Debug)]
pub struct StrategyStatePersistenceHandle {
    sender: mpsc::Sender<StrategyStatePersistenceRequest>,
    healthy: Arc<AtomicBool>,
    writer_id: Arc<str>,
    run_id: Uuid,
}

impl StrategyStatePersistenceHandle {
    #[must_use]
    pub fn spawn(
        storage: Arc<OperationalRepository>,
        account_id: String,
        writer_id: String,
        run_id: Uuid,
    ) -> Self {
        let (sender, mut receiver) =
            mpsc::channel::<StrategyStatePersistenceRequest>(STATE_PERSISTENCE_QUEUE_CAPACITY);
        let healthy = Arc::new(AtomicBool::new(true));
        let sink_healthy = Arc::clone(&healthy);
        let sink_writer_id = writer_id.clone();
        tokio::spawn(async move {
            while let Some(request) = receiver.recv().await {
                match request {
                    StrategyStatePersistenceRequest::Persist { mutation, state } => {
                        match persist_strategy_state_mutation(
                            &storage,
                            &account_id,
                            &mutation,
                            &state,
                        )
                        .await
                        {
                            Ok(result) => {
                                log::debug!(
                                    "Persisted Alpaca strategy state mutation: account_id={} event_type={} event_id={} status={:?} version={}",
                                    account_id,
                                    mutation.event_type,
                                    mutation.event_id,
                                    result.status,
                                    result.state_version
                                );
                            }
                            Err(error) => {
                                sink_healthy.store(false, Ordering::Release);
                                log::error!(
                                    "Alpaca strategy-state persistence failed: account_id={} writer_id={} event_type={} event_id={} error={error:#}",
                                    account_id,
                                    sink_writer_id,
                                    mutation.event_type,
                                    mutation.event_id
                                );
                                emit_operator_event(
                                    "strategy_state_persistence_error",
                                    json!({
                                        "reason": "write_failed",
                                        "account_id": account_id.clone(),
                                        "writer_id": sink_writer_id.clone(),
                                        "event_type": mutation.event_type.clone(),
                                        "event_id": mutation.event_id.to_string(),
                                        "error": error.to_string(),
                                    }),
                                );
                            }
                        }
                    }
                    StrategyStatePersistenceRequest::Flush { ack } => {
                        let _ = ack.send(());
                    }
                }
            }
            sink_healthy.store(false, Ordering::Release);
        });

        Self {
            sender,
            healthy,
            writer_id: Arc::from(writer_id),
            run_id,
        }
    }

    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub fn mark_unhealthy(&self) {
        self.healthy.store(false, Ordering::Release);
    }

    pub fn persist(
        &self,
        mut mutation: StrategyStateMutation,
        state: StrategyState,
    ) -> anyhow::Result<()> {
        mutation
            .writer_id
            .get_or_insert_with(|| self.writer_id.to_string());
        mutation.run_id.get_or_insert(self.run_id);
        self.sender
            .try_send(StrategyStatePersistenceRequest::Persist { mutation, state })
            .map_err(|error| {
                self.healthy.store(false, Ordering::Release);
                emit_operator_event(
                    "strategy_state_persistence_error",
                    json!({
                        "reason": "queue_unavailable",
                        "writer_id": self.writer_id.as_ref(),
                        "run_id": self.run_id.to_string(),
                        "error": error.to_string(),
                    }),
                );
                anyhow::anyhow!("strategy-state persistence queue unavailable: {error}")
            })
    }

    pub async fn flush(&self) -> anyhow::Result<()> {
        let (ack, receiver) = oneshot::channel();
        self.sender
            .send(StrategyStatePersistenceRequest::Flush { ack })
            .await
            .map_err(|error| {
                self.healthy.store(false, Ordering::Release);
                anyhow::anyhow!("strategy-state persistence queue closed before flush: {error}")
            })?;
        tokio::time::timeout(
            Duration::from_secs(STATE_PERSISTENCE_FLUSH_TIMEOUT_SECS),
            receiver,
        )
        .await
        .map_err(|_| anyhow::anyhow!("strategy-state persistence flush timed out"))?
        .map_err(|error| anyhow::anyhow!("strategy-state persistence flush failed: {error}"))
    }
}

#[derive(Debug)]
enum StrategyStatePersistenceRequest {
    Persist {
        mutation: StrategyStateMutation,
        state: StrategyState,
    },
    Flush {
        ack: oneshot::Sender<()>,
    },
}

pub fn start_runtime_lease_heartbeat(
    storage: Arc<OperationalRepository>,
    account_id: String,
    run_id: Uuid,
    ttl: Duration,
    state_persistence: Option<StrategyStatePersistenceHandle>,
    candidate_ledger_persistence: Option<CandidateLedgerPersistenceHandle>,
) {
    let interval = Duration::from_secs((ttl.as_secs() / 3).max(5));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            match heartbeat_runtime_lease(&storage, &account_id, run_id, ttl).await {
                Ok(true) => {}
                Ok(false) => {
                    mark_persistence_unhealthy(
                        state_persistence.as_ref(),
                        candidate_ledger_persistence.as_ref(),
                    );
                    log::error!(
                        "Alpaca runtime lease heartbeat lost ownership: account_id={} run_id={}",
                        account_id,
                        run_id
                    );
                    emit_operator_event(
                        "runtime_lease_lost",
                        json!({
                            "account_id": account_id.clone(),
                            "run_id": run_id.to_string(),
                        }),
                    );
                    break;
                }
                Err(error) => {
                    mark_persistence_unhealthy(
                        state_persistence.as_ref(),
                        candidate_ledger_persistence.as_ref(),
                    );
                    log::error!(
                        "Alpaca runtime lease heartbeat failed: account_id={} run_id={} error={error:#}",
                        account_id,
                        run_id
                    );
                    emit_operator_event(
                        "runtime_lease_error",
                        json!({
                            "account_id": account_id.clone(),
                            "run_id": run_id.to_string(),
                            "error": error.to_string(),
                        }),
                    );
                    break;
                }
            }
        }
    });
}

fn mark_persistence_unhealthy(
    state_persistence: Option<&StrategyStatePersistenceHandle>,
    candidate_ledger_persistence: Option<&CandidateLedgerPersistenceHandle>,
) {
    if let Some(state_persistence) = state_persistence {
        state_persistence.mark_unhealthy();
    }
    if let Some(candidate_ledger_persistence) = candidate_ledger_persistence {
        candidate_ledger_persistence.mark_unhealthy();
    }
}
