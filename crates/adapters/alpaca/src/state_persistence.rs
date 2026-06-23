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
    runtime::StrategyState,
    storage::{StorageRepository, StrategyStateMutation, persist_strategy_state_mutation},
};

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
) {
    let interval = Duration::from_secs((ttl.as_secs() / 3).max(5));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            match crate::storage::heartbeat_runtime_lease(&storage, &account_id, run_id, ttl).await
            {
                Ok(true) => {}
                Ok(false) => {
                    log::error!(
                        "Alpaca runtime lease heartbeat lost ownership: account_id={} run_id={}",
                        account_id,
                        run_id
                    );
                    break;
                }
                Err(error) => {
                    log::error!(
                        "Alpaca runtime lease heartbeat failed: account_id={} run_id={} error={error:#}",
                        account_id,
                        run_id
                    );
                    break;
                }
            }
        }
    });
}
