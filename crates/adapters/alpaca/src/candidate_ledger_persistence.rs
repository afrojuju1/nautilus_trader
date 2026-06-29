//! Async candidate-ledger persistence boundary for Alpaca live strategies.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};

use nautilus_infrastructure::sql::operational::{
    OperationalRepository, append_candidate_ledger_record,
};

use crate::runtime::emit_operator_event;

const CANDIDATE_LEDGER_QUEUE_CAPACITY: usize = 256;
const CANDIDATE_LEDGER_FLUSH_TIMEOUT_SECS: u64 = 5;

#[derive(Clone, Debug)]
pub struct CandidateLedgerPersistenceHandle {
    sender: mpsc::Sender<CandidateLedgerPersistenceRequest>,
    healthy: Arc<AtomicBool>,
    account_id: Arc<str>,
}

impl CandidateLedgerPersistenceHandle {
    #[must_use]
    pub fn spawn(storage: Arc<OperationalRepository>, account_id: String) -> Self {
        let (sender, mut receiver) =
            mpsc::channel::<CandidateLedgerPersistenceRequest>(CANDIDATE_LEDGER_QUEUE_CAPACITY);
        let healthy = Arc::new(AtomicBool::new(true));
        let sink_healthy = Arc::clone(&healthy);
        let sink_account_id = account_id.clone();
        tokio::spawn(async move {
            while let Some(request) = receiver.recv().await {
                match request {
                    CandidateLedgerPersistenceRequest::Append {
                        trade_date,
                        record_type,
                        payload,
                    } => {
                        if let Err(error) = append_candidate_ledger_record(
                            &storage,
                            &sink_account_id,
                            &trade_date,
                            &record_type,
                            payload,
                        )
                        .await
                        {
                            sink_healthy.store(false, Ordering::Release);
                            log::error!(
                                "Alpaca candidate-ledger persistence failed: account_id={} trade_date={} record_type={} error={error:#}",
                                sink_account_id,
                                trade_date,
                                record_type
                            );
                            emit_operator_event(
                                "candidate_ledger_error",
                                json!({
                                    "reason": "write_failed",
                                    "account_id": sink_account_id.clone(),
                                    "trade_date": trade_date,
                                    "record_type": record_type,
                                    "error": error.to_string(),
                                }),
                            );
                        }
                    }
                    CandidateLedgerPersistenceRequest::Flush { ack } => {
                        let _ = ack.send(());
                    }
                }
            }
            sink_healthy.store(false, Ordering::Release);
        });

        Self {
            sender,
            healthy,
            account_id: Arc::from(account_id),
        }
    }

    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    pub fn mark_unhealthy(&self) {
        self.healthy.store(false, Ordering::Release);
    }

    pub fn append(
        &self,
        trade_date: &str,
        record_type: &str,
        payload: Value,
    ) -> anyhow::Result<()> {
        self.sender
            .try_send(CandidateLedgerPersistenceRequest::Append {
                trade_date: trade_date.to_string(),
                record_type: record_type.to_string(),
                payload,
            })
            .map_err(|error| {
                self.healthy.store(false, Ordering::Release);
                emit_operator_event(
                    "candidate_ledger_error",
                    json!({
                        "reason": "queue_unavailable",
                        "account_id": self.account_id.as_ref(),
                        "trade_date": trade_date,
                        "record_type": record_type,
                        "error": error.to_string(),
                    }),
                );
                anyhow::anyhow!("candidate-ledger persistence queue unavailable: {error}")
            })
    }

    pub fn append_candidate_alert(
        &self,
        trade_date: &str,
        alert_type: &str,
        severity: &str,
        alert_key: String,
        payload: Value,
    ) -> anyhow::Result<()> {
        let mut record = match payload {
            Value::Object(fields) => fields,
            value => {
                let mut fields = Map::new();
                fields.insert("payload".to_string(), value);
                fields
            }
        };
        record.insert(
            "alert_type".to_string(),
            Value::String(alert_type.to_string()),
        );
        record.insert("severity".to_string(), Value::String(severity.to_string()));
        record.insert("alert_key".to_string(), Value::String(alert_key));
        self.append(trade_date, "candidate_alert", Value::Object(record))
    }

    pub async fn flush(&self) -> anyhow::Result<()> {
        let (ack, receiver) = oneshot::channel();
        self.sender
            .send(CandidateLedgerPersistenceRequest::Flush { ack })
            .await
            .map_err(|error| {
                self.healthy.store(false, Ordering::Release);
                anyhow::anyhow!("candidate-ledger persistence queue closed before flush: {error}")
            })?;
        tokio::time::timeout(
            Duration::from_secs(CANDIDATE_LEDGER_FLUSH_TIMEOUT_SECS),
            receiver,
        )
        .await
        .map_err(|_| anyhow::anyhow!("candidate-ledger persistence flush timed out"))?
        .map_err(|error| anyhow::anyhow!("candidate-ledger persistence flush failed: {error}"))
    }
}

#[derive(Debug)]
enum CandidateLedgerPersistenceRequest {
    Append {
        trade_date: String,
        record_type: String,
        payload: Value,
    },
    Flush {
        ack: oneshot::Sender<()>,
    },
}
