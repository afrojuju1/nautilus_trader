//! Async strategy-state persistence boundary for Alpaca live strategies.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    candidate_ledger_persistence::CandidateLedgerPersistenceHandle,
    runtime::{StrategyState, emit_operator_event},
    storage::{StorageRepository, StrategyStateMutation, persist_strategy_state_mutation},
};
use serde_json::json;

const STATE_PERSISTENCE_QUEUE_CAPACITY: usize = 64;

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
        storage: Arc<StorageRepository>,
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
                match persist_strategy_state_mutation(
                    &storage,
                    &account_id,
                    &request.mutation,
                    &request.state,
                )
                .await
                {
                    Ok(result) => {
                        log::debug!(
                            "Persisted Alpaca strategy state mutation: account_id={} event_type={} event_id={} status={:?} version={}",
                            account_id,
                            request.mutation.event_type,
                            request.mutation.event_id,
                            result.status,
                            result.snapshot_version
                        );
                    }
                    Err(error) => {
                        sink_healthy.store(false, Ordering::Release);
                        log::error!(
                            "Alpaca strategy-state persistence failed: account_id={} writer_id={} event_type={} event_id={} error={error:#}",
                            account_id,
                            sink_writer_id,
                            request.mutation.event_type,
                            request.mutation.event_id
                        );
                        emit_operator_event(
                            "strategy_state_persistence_error",
                            json!({
                                "reason": "write_failed",
                                "account_id": account_id.clone(),
                                "writer_id": sink_writer_id.clone(),
                                "event_type": request.mutation.event_type.clone(),
                                "event_id": request.mutation.event_id.to_string(),
                                "error": error.to_string(),
                            }),
                        );
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
            .try_send(StrategyStatePersistenceRequest { mutation, state })
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
}

#[derive(Debug)]
struct StrategyStatePersistenceRequest {
    mutation: StrategyStateMutation,
    state: StrategyState,
}

pub fn start_runtime_lease_heartbeat(
    storage: Arc<StorageRepository>,
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
            match crate::storage::heartbeat_runtime_lease(&storage, &account_id, run_id, ttl).await
            {
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
