//! Read-only Nautilus actor for option-chain candidate evidence.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex, MutexGuard, mpsc},
    thread::{self, JoinHandle},
    time::{Duration as StdDuration, Instant},
};

use chrono::{Datelike, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use nautilus_common::{
    actor::{DataActor, DataActorConfig, DataActorCore},
    nautilus_actor,
    timer::TimeEvent,
};
use nautilus_core::{Params, UnixNanos};
use nautilus_model::{
    data::{
        Bar, BarSpecification, BarType,
        option_chain::{OptionChainSlice, StrikeRange},
    },
    enums::{AggregationSource, BarAggregation, PriceType},
    identifiers::{ActorId, ClientId, InstrumentId, OptionSeriesId},
    instruments::{Instrument, InstrumentAny},
};
use nautilus_trading::options::{
    candidates::{
        CreditSpreadKind, DebitSpreadKind, DebitSpreadScannerConfig, IronCondorScannerConfig,
        NakedOptionCapitalContext, NakedOptionKind, NakedOptionScannerConfig,
        PutCreditScannerConfig,
    },
    entries::{
        SelectedDebitEntry, SelectedEntry, SelectedIronCondorEntry, SelectedNakedOptionEntry,
        SelectedOptionsEntry, credit_spread_strategy_name, debit_spread_strategy_name,
        naked_option_strategy_name,
    },
    regime::{
        RegimeContext, RegimeEvent, RegimeFeatureConfig, RegimeFeatureData, RegimeFeatureInputs,
        RegimeFeatureSnapshot, RegimeRoutingSummary, apply_regime_routing, insert_regime_context,
        regime_context_from_features, regime_feature_snapshot_from_option_chain,
    },
};
use serde_json::{Value, json};

use crate::{
    candidate_ledger_persistence::CandidateLedgerPersistenceHandle,
    candidate_payloads::selected_entry_candidate_ledger_payload,
    common::consts::{
        ALPACA_OPTION_CHAIN_EXPIRATION_PARAM, ALPACA_OPTION_CHAIN_UNDERLYING_PARAM, ALPACA_VENUE,
    },
    earnings::EarningsEvent,
    option_chain_candidates::{
        OptionChainCandidateInput, option_chain_candidate_input, scan_credit_spread_option_chain,
        scan_debit_spread_option_chain, scan_iron_condor_option_chain, scan_naked_option_chain,
    },
    options_account_strategy::OptionsCandidateData,
    options_runtime::{
        AlpacaOptionsRuntimeConfig, AlpacaOptionsStrategyFamily, AlpacaOptionsStrategyProfile,
        AlpacaOptionsStrategyScannerConfig, OptionsCandidateSet, OptionsScanOutcome,
        OptionsScanReport,
    },
    runtime::emit_operator_event,
};

const SCAN_RESULT_TIMER: &str = "alpaca_option_chain_scan_results";
const DEFAULT_SCAN_QUEUE_CAPACITY: usize = 4;
const DEFAULT_SCAN_WORKER_THREADS: usize = 2;
const DEFAULT_SCAN_RESULT_DRAIN_INTERVAL_MS: u64 = 250;
const DEFAULT_SCAN_MAX_RESULT_AGE_MS: u64 = 15_000;

/// Read-only scan settings for candidate discovery from option-chain slices.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionChainCandidateScanConfig {
    /// Resolved strategy profiles. Runtime configs populate this; defaults mirror family flags.
    pub strategy_profiles: Vec<AlpacaOptionsStrategyProfile>,
    /// Enabled credit-spread kinds.
    pub spread_kinds: Vec<CreditSpreadKind>,
    /// Whether to scan iron-condor candidates.
    pub iron_condor_enabled: bool,
    /// Enabled debit-spread kinds.
    pub debit_kinds: Vec<DebitSpreadKind>,
    /// Enabled naked-option kinds.
    pub naked_kinds: Vec<NakedOptionKind>,
    /// Credit-spread scanner configuration.
    pub credit_scanner: PutCreditScannerConfig,
    /// Iron-condor scanner configuration.
    pub iron_condor_scanner: IronCondorScannerConfig,
    /// Debit-spread scanner configuration.
    pub debit_scanner: DebitSpreadScannerConfig,
    /// Naked-option scanner configuration.
    pub naked_scanner: NakedOptionScannerConfig,
    /// Naked-option scanner configuration for 1-3 DTE profiles.
    pub naked_1_3dte_scanner: NakedOptionScannerConfig,
    /// Optional account buying-power context for naked-option ranking.
    pub options_buying_power: Option<f64>,
    /// Quantity used for buying-power estimates.
    pub quantity: u64,
    /// Maximum ranked candidates to write per scanner result. `0` means all candidates.
    pub candidate_ledger_max_candidates: usize,
    /// Timezone used to derive the strategy trade date for daily risk limits.
    pub trade_date_timezone: Tz,
    /// Read-only regime feature snapshot settings.
    pub regime_features: RegimeFeatureConfig,
    /// Days of underlying bars to request for regime feature windows.
    pub underlying_bar_lookback_days: i64,
    /// Maximum cached underlying bars per underlying.
    pub underlying_bar_limit: usize,
    /// Approved earnings events used by event-load features.
    pub event_shock_earnings_events: Vec<EarningsEvent>,
    /// Calendar days before an earnings report considered event load.
    pub event_shock_block_days_before_earnings: i64,
    /// Calendar days after an earnings report considered event load.
    pub event_shock_block_days_after_earnings: i64,
}

impl Default for OptionChainCandidateScanConfig {
    fn default() -> Self {
        Self {
            strategy_profiles: Vec::new(),
            spread_kinds: vec![CreditSpreadKind::Put],
            iron_condor_enabled: false,
            debit_kinds: Vec::new(),
            naked_kinds: Vec::new(),
            credit_scanner: PutCreditScannerConfig::default(),
            iron_condor_scanner: IronCondorScannerConfig::default(),
            debit_scanner: DebitSpreadScannerConfig::default(),
            naked_scanner: NakedOptionScannerConfig::default(),
            naked_1_3dte_scanner: NakedOptionScannerConfig {
                min_dte: 1,
                max_dte: 3,
                short_delta_min: 0.08,
                short_delta_max: 0.18,
                min_open_interest: 300,
                max_spread_pct: 0.15,
                min_credit: 0.10,
                min_daily_volume: 0,
                min_annualized_premium_yield: 0.20,
                min_breakeven_pop: 0.70,
                max_probability_of_touch: 0.65,
                min_distance_to_breakeven_pct: 0.0025,
                min_expected_move_coverage: 0.60,
                min_score: 60.0,
                ..NakedOptionScannerConfig::default()
            },
            options_buying_power: None,
            quantity: 1,
            candidate_ledger_max_candidates: 10,
            trade_date_timezone: chrono_tz::UTC,
            regime_features: RegimeFeatureConfig::default(),
            underlying_bar_lookback_days: 90,
            underlying_bar_limit: 120,
            event_shock_earnings_events: Vec::new(),
            event_shock_block_days_before_earnings: 1,
            event_shock_block_days_after_earnings: 1,
        }
    }
}

/// Actor configuration for read-only option-chain candidate scans.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionChainCandidateScanActorConfig {
    /// Actor ID.
    pub actor_id: Option<ActorId>,
    /// Option series subscriptions.
    pub series: Vec<OptionSeriesId>,
    /// Strike range for every subscribed series.
    pub strike_range: StrikeRange,
    /// Optional snapshot interval in milliseconds.
    pub snapshot_interval_ms: Option<u64>,
    /// Optional data client ID.
    pub client_id: Option<ClientId>,
    /// Whether to request Alpaca option instruments before subscribing to option-chain slices.
    pub bootstrap_instruments: bool,
    /// Candidate scan settings.
    pub scan: OptionChainCandidateScanConfig,
    /// Maximum pending option-chain scan jobs. When full, the oldest job is dropped.
    pub scan_queue_capacity: usize,
    /// Number of background scan worker threads. Values below `1` are treated as `1`.
    pub scan_worker_threads: usize,
    /// Interval in milliseconds for draining completed scan results back onto the actor thread.
    pub scan_result_drain_interval_ms: u64,
    /// Maximum wall-clock age in milliseconds for a completed scan result. `0` disables age drops.
    pub scan_max_result_age_ms: u64,
}

impl Default for OptionChainCandidateScanActorConfig {
    fn default() -> Self {
        Self {
            actor_id: Some(ActorId::from("ALPACA-OPPORTUNITY-SCAN")),
            series: Vec::new(),
            strike_range: StrikeRange::AtmRelative {
                strikes_above: 10,
                strikes_below: 10,
            },
            snapshot_interval_ms: Some(5_000),
            client_id: None,
            bootstrap_instruments: false,
            scan: OptionChainCandidateScanConfig::default(),
            scan_queue_capacity: DEFAULT_SCAN_QUEUE_CAPACITY,
            scan_worker_threads: DEFAULT_SCAN_WORKER_THREADS,
            scan_result_drain_interval_ms: DEFAULT_SCAN_RESULT_DRAIN_INTERVAL_MS,
            scan_max_result_age_ms: DEFAULT_SCAN_MAX_RESULT_AGE_MS,
        }
    }
}

/// Read-only actor that ranks option-chain candidates and emits operator evidence.
#[derive(Debug)]
pub struct OptionChainCandidateScanActor {
    core: DataActorCore,
    config: OptionChainCandidateScanActorConfig,
    candidate_ledger_persistence: Option<CandidateLedgerPersistenceHandle>,
    subscribed_series: BTreeSet<OptionSeriesId>,
    underlying_bar_types: BTreeMap<String, BarType>,
    latest_underlying_bars: BTreeMap<String, Vec<Bar>>,
    latest_candidates: Option<OptionsCandidateSet>,
    scan_workers: Option<ScanWorkerPool>,
    latest_enqueued_scan_sequence: u64,
    latest_published_scan_sequence: u64,
}

nautilus_actor!(OptionChainCandidateScanActor);

impl OptionChainCandidateScanActor {
    /// Creates a new read-only option-chain candidate scan actor.
    #[must_use]
    pub fn new(config: OptionChainCandidateScanActorConfig) -> Self {
        let core = DataActorCore::new(DataActorConfig {
            actor_id: config.actor_id.clone(),
            ..Default::default()
        });
        Self {
            core,
            config,
            candidate_ledger_persistence: None,
            subscribed_series: BTreeSet::new(),
            underlying_bar_types: BTreeMap::new(),
            latest_underlying_bars: BTreeMap::new(),
            latest_candidates: None,
            scan_workers: None,
            latest_enqueued_scan_sequence: 0,
            latest_published_scan_sequence: 0,
        }
    }

    /// Adds a bounded candidate-ledger persistence sink for operator evidence.
    #[must_use]
    pub fn with_candidate_ledger_persistence(
        mut self,
        persistence: CandidateLedgerPersistenceHandle,
    ) -> Self {
        self.candidate_ledger_persistence = Some(persistence);
        self
    }

    /// Returns the most recent candidate set produced by this actor.
    #[must_use]
    pub fn latest_candidates(&self) -> Option<&OptionsCandidateSet> {
        self.latest_candidates.as_ref()
    }

    fn subscribe_series(&mut self, series_id: OptionSeriesId) {
        if !self.subscribed_series.insert(series_id) {
            return;
        }

        let cached = self
            .cache()
            .instruments(&series_id.venue, Some(&series_id.underlying))
            .into_iter()
            .filter(|instrument| instrument_belongs_to_series(instrument, &series_id))
            .count();
        log::info!(
            "Subscribing to Alpaca option-chain series {series_id} with {cached} cached instruments"
        );

        self.subscribe_option_chain(
            series_id,
            self.config.strike_range.clone(),
            self.config.snapshot_interval_ms,
            self.config.client_id,
            None,
        );
    }

    fn request_series_instruments(&mut self, series_id: OptionSeriesId) -> anyhow::Result<()> {
        let mut params = Params::new();
        params.insert(
            ALPACA_OPTION_CHAIN_UNDERLYING_PARAM.to_string(),
            json!(series_id.underlying.as_str()),
        );
        params.insert(
            ALPACA_OPTION_CHAIN_EXPIRATION_PARAM.to_string(),
            json!(
                series_id
                    .expiration_ns
                    .to_datetime_utc()
                    .date_naive()
                    .format("%Y-%m-%d")
                    .to_string()
            ),
        );

        log::info!("Requesting Alpaca option instruments for {series_id}");
        self.request_instruments(
            Some(series_id.venue),
            None,
            None,
            self.config.client_id,
            Some(params),
        )?;
        Ok(())
    }

    fn request_underlying_bars(&mut self, series_id: OptionSeriesId) -> anyhow::Result<()> {
        let underlying = series_id.underlying.to_string();
        let bar_type = underlying_daily_bar_type(&underlying);
        if self
            .underlying_bar_types
            .insert(underlying.clone(), bar_type)
            .is_some()
        {
            return Ok(());
        }

        let end = Utc::now();
        let start = end - Duration::days(self.config.scan.underlying_bar_lookback_days.max(1));
        let limit = NonZeroUsize::new(self.config.scan.underlying_bar_limit.max(1));
        log::info!(
            "Requesting Alpaca underlying bars for {underlying}: bar_type={bar_type} start={} end={}",
            start.to_rfc3339(),
            end.to_rfc3339(),
        );
        self.request_bars(
            bar_type,
            Some(start),
            Some(end),
            limit,
            self.config.client_id,
            None,
        )?;
        Ok(())
    }

    fn store_underlying_bars(&mut self, bar_type: BarType, mut bars: Vec<Bar>) {
        let Some(underlying) = self.underlying_for_bar_type(bar_type) else {
            return;
        };
        if bars.is_empty() {
            return;
        }

        bars.sort_by_key(|bar| bar.ts_event);
        let limit = self.config.scan.underlying_bar_limit.max(1);
        if bars.len() > limit {
            bars = bars[bars.len() - limit..].to_vec();
        }
        self.latest_underlying_bars.insert(underlying, bars);
    }

    fn store_underlying_bar(&mut self, bar: Bar) {
        let Some(underlying) = self.underlying_for_bar_type(bar.bar_type) else {
            return;
        };
        let limit = self.config.scan.underlying_bar_limit.max(1);
        let bars = self.latest_underlying_bars.entry(underlying).or_default();
        bars.retain(|cached| cached.ts_event != bar.ts_event);
        bars.push(bar);
        bars.sort_by_key(|cached| cached.ts_event);
        if bars.len() > limit {
            let excess = bars.len() - limit;
            bars.drain(0..excess);
        }
    }

    fn underlying_for_bar_type(&self, bar_type: BarType) -> Option<String> {
        self.underlying_bar_types
            .iter()
            .find_map(|(underlying, cached)| (*cached == bar_type).then(|| underlying.clone()))
    }

    fn record_candidate_evidence(
        &self,
        trade_date: &str,
        candidates: &OptionsCandidateSet,
        scan_payload: Value,
        regime_context: &RegimeContext,
    ) {
        let Some(persistence) = &self.candidate_ledger_persistence else {
            return;
        };
        if let Err(error) = persistence.append(trade_date, "scanner_result", scan_payload) {
            log::error!("Failed to enqueue Alpaca option-chain scanner evidence: {error:#}");
        }

        let candidate_limit = candidate_ledger_candidate_limit(
            self.config.scan.candidate_ledger_max_candidates,
            candidates.ranked_entries().len(),
        );
        for (index, entry) in candidates
            .ranked_entries()
            .iter()
            .take(candidate_limit)
            .enumerate()
        {
            let mut payload = selected_entry_candidate_ledger_payload(
                entry,
                self.config.scan.options_buying_power,
                Some(index + 1),
            );
            insert_regime_context(&mut payload, Some(regime_context));
            if let Err(error) = persistence.append(trade_date, "candidate", payload) {
                log::error!("Failed to enqueue Alpaca option-chain candidate evidence: {error:#}");
                break;
            }
        }
    }

    fn start_scan_workers(&mut self) -> anyhow::Result<()> {
        if self.scan_workers.is_some() {
            return Ok(());
        }

        let queue_capacity = self.config.scan_queue_capacity.max(1);
        let worker_count = self.config.scan_worker_threads.max(1);
        self.scan_workers = Some(ScanWorkerPool::start(
            self.config.scan.clone(),
            queue_capacity,
            worker_count,
        )?);
        emit_operator_event(
            "option_chain_scan_worker_pool",
            json!({
                "event": "started",
                "queue_capacity": queue_capacity,
                "worker_threads": worker_count,
                "result_drain_interval_ms": self.config.scan_result_drain_interval_ms,
                "max_result_age_ms": self.config.scan_max_result_age_ms,
            }),
        );
        Ok(())
    }

    fn stop_scan_workers(&mut self) {
        let Some(pool) = self.scan_workers.take() else {
            return;
        };
        let worker_count = pool.worker_count();
        let queue_capacity = pool.queue_capacity();
        let messages = pool.shutdown();
        emit_operator_event(
            "option_chain_scan_worker_pool",
            json!({
                "event": "stopped",
                "queue_capacity": queue_capacity,
                "worker_threads": worker_count,
                "drained_results": messages.len(),
            }),
        );
        for message in messages {
            self.handle_scan_worker_message(message);
        }
    }

    fn enqueue_scan_job(
        &mut self,
        slice: &OptionChainSlice,
        trade_date: String,
        ts_init: UnixNanos,
    ) {
        let Some(pool) = &self.scan_workers else {
            emit_operator_event(
                "option_chain_scan_worker_error",
                json!({
                    "reason": "worker_pool_not_started",
                    "series_id": slice.series_id.to_string(),
                    "source_ts_event": slice.ts_event.as_u64(),
                    "source_ts_init": slice.ts_init.as_u64(),
                }),
            );
            return;
        };

        self.latest_enqueued_scan_sequence = self.latest_enqueued_scan_sequence.saturating_add(1);
        let underlying = slice.series_id.underlying.to_string();
        let job = ScanJob {
            sequence: self.latest_enqueued_scan_sequence,
            slice: slice.clone(),
            trade_date,
            ts_init,
            underlying_bars: self
                .latest_underlying_bars
                .get(&underlying)
                .cloned()
                .unwrap_or_default(),
            enqueued_at: Instant::now(),
        };
        let summary = job.summary();
        match pool.enqueue(job) {
            ScanEnqueueOutcome::Enqueued {
                depth,
                dropped_oldest,
            } => {
                if let Some(dropped) = dropped_oldest {
                    emit_operator_event(
                        "option_chain_scan_queue_overflow",
                        json!({
                            "overflow_policy": "drop_oldest",
                            "queue_capacity": pool.queue_capacity(),
                            "queue_depth": depth,
                            "dropped_sequence": dropped.sequence,
                            "dropped_series_id": dropped.series_id,
                            "dropped_source_ts_event": dropped.source_ts_event.as_u64(),
                            "dropped_source_ts_init": dropped.source_ts_init.as_u64(),
                            "enqueued_sequence": summary.sequence,
                            "enqueued_series_id": summary.series_id,
                            "enqueued_source_ts_event": summary.source_ts_event.as_u64(),
                            "enqueued_source_ts_init": summary.source_ts_init.as_u64(),
                        }),
                    );
                }
                emit_operator_event(
                    "option_chain_scan_queue",
                    json!({
                        "event": "enqueued",
                        "queue_capacity": pool.queue_capacity(),
                        "queue_depth": depth,
                        "sequence": summary.sequence,
                        "series_id": summary.series_id,
                        "source_ts_event": summary.source_ts_event.as_u64(),
                        "source_ts_init": summary.source_ts_init.as_u64(),
                    }),
                );
            }
            ScanEnqueueOutcome::Closed => {
                emit_operator_event(
                    "option_chain_scan_worker_error",
                    json!({
                        "reason": "worker_queue_closed",
                        "sequence": summary.sequence,
                        "series_id": summary.series_id,
                        "source_ts_event": summary.source_ts_event.as_u64(),
                        "source_ts_init": summary.source_ts_init.as_u64(),
                    }),
                );
            }
        }
    }

    fn drain_scan_results(&mut self) {
        loop {
            let message = self
                .scan_workers
                .as_ref()
                .and_then(ScanWorkerPool::try_recv);
            let Some(message) = message else {
                break;
            };
            self.handle_scan_worker_message(message);
        }
    }

    fn handle_scan_worker_message(&mut self, message: ScanWorkerMessage) {
        match message {
            ScanWorkerMessage::Result(result) => self.handle_scan_result(result),
            ScanWorkerMessage::Error(error) => {
                emit_operator_event(
                    "option_chain_scan_worker_error",
                    json!({
                        "reason": error.reason,
                        "worker_index": error.worker_index,
                        "sequence": error.source.sequence,
                        "series_id": error.source.series_id,
                        "source_ts_event": error.source.source_ts_event.as_u64(),
                        "source_ts_init": error.source.source_ts_init.as_u64(),
                        "queue_latency_ms": duration_ms_u64(error.started_at.duration_since(error.enqueued_at)),
                        "worker_latency_ms": duration_ms_u64(error.completed_at.duration_since(error.started_at)),
                    }),
                );
            }
        }
    }

    fn handle_scan_result(&mut self, result: ScanWorkerResult) {
        if let Some(reason) = self.scan_result_drop_reason(&result) {
            emit_operator_event(
                "option_chain_scan_result_dropped",
                json!({
                    "reason": reason,
                    "worker_index": result.worker_index,
                    "sequence": result.source.sequence,
                    "latest_enqueued_sequence": self.latest_enqueued_scan_sequence,
                    "latest_published_sequence": self.latest_published_scan_sequence,
                    "series_id": result.source.series_id.to_string(),
                    "source_ts_event": result.source.source_ts_event.as_u64(),
                    "source_ts_init": result.source.source_ts_init.as_u64(),
                    "candidate_ts_init": result.source.candidate_ts_init.as_u64(),
                    "queue_latency_ms": duration_ms_u64(result.started_at.duration_since(result.enqueued_at)),
                    "scan_latency_ms": duration_ms_u64(result.completed_at.duration_since(result.started_at)),
                    "total_latency_ms": duration_ms_u64(result.completed_at.duration_since(result.enqueued_at)),
                }),
            );
            return;
        }

        emit_operator_event(
            "option_chain_scan_result",
            json!({
                "event": "published",
                "worker_index": result.worker_index,
                "sequence": result.source.sequence,
                "series_id": result.source.series_id.to_string(),
                "source_ts_event": result.source.source_ts_event.as_u64(),
                "source_ts_init": result.source.source_ts_init.as_u64(),
                "candidate_ts_init": result.source.candidate_ts_init.as_u64(),
                "queue_latency_ms": duration_ms_u64(result.started_at.duration_since(result.enqueued_at)),
                "scan_latency_ms": duration_ms_u64(result.completed_at.duration_since(result.started_at)),
                "total_latency_ms": duration_ms_u64(result.completed_at.duration_since(result.enqueued_at)),
            }),
        );
        self.publish_scan_result(result);
    }

    fn scan_result_drop_reason(&self, result: &ScanWorkerResult) -> Option<&'static str> {
        if result.source.sequence <= self.latest_published_scan_sequence {
            return Some("already_published_or_older");
        }
        if result.source.sequence < self.latest_enqueued_scan_sequence {
            return Some("newer_scan_enqueued");
        }
        if self.config.scan_max_result_age_ms > 0
            && result.completed_at.duration_since(result.enqueued_at)
                > StdDuration::from_millis(self.config.scan_max_result_age_ms)
        {
            return Some("max_result_age_exceeded");
        }
        None
    }

    fn publish_scan_result(&mut self, result: ScanWorkerResult) {
        let mut candidates = result.candidates;
        let routing_summary =
            apply_regime_routing(&mut candidates.ranked_entries, &result.regime_context);
        let evidence_payload = candidate_event_payload(
            &result.source,
            &candidates,
            &result.regime_context,
            &routing_summary,
        );

        emit_operator_event(
            "regime_feature_snapshot",
            result.feature_snapshot.to_json_value(),
        );
        let regime_data = RegimeFeatureData::new(result.feature_snapshot).into_custom_data();
        self.publish_data(&regime_data.data_type, &regime_data);

        emit_operator_event("option_chain_candidate_scan", evidence_payload.clone());
        self.record_candidate_evidence(
            &result.source.trade_date,
            &candidates,
            evidence_payload,
            &result.regime_context,
        );
        let data = OptionsCandidateData::new(
            candidates.clone(),
            Some(result.regime_context),
            result.source.source_ts_event,
            result.source.candidate_ts_init,
        )
        .into_custom_data();
        self.publish_data(&data.data_type, &data);
        self.latest_published_scan_sequence = result.source.sequence;
        self.latest_candidates = Some(candidates);
    }
}

impl DataActor for OptionChainCandidateScanActor {
    fn on_start(&mut self) -> anyhow::Result<()> {
        if self.config.series.is_empty() {
            log::warn!("Option-chain candidate scan actor has no series subscriptions");
            return Ok(());
        }

        self.start_scan_workers()?;
        self.clock().set_timer(
            SCAN_RESULT_TIMER,
            StdDuration::from_millis(self.config.scan_result_drain_interval_ms.max(1)),
            None,
            None,
            None,
            None,
            None,
        )?;

        for series_id in self.config.series.clone() {
            if self.config.bootstrap_instruments {
                self.request_series_instruments(series_id)?;
            } else {
                self.subscribe_series(series_id);
            }
            if let Err(error) = self.request_underlying_bars(series_id) {
                log::warn!("Failed to request Alpaca underlying bars for {series_id}: {error:#}");
            }
        }
        Ok(())
    }

    fn on_instrument(&mut self, instrument: &InstrumentAny) -> anyhow::Result<()> {
        if !self.config.bootstrap_instruments {
            return Ok(());
        }

        for series_id in self.config.series.clone() {
            if instrument_belongs_to_series(instrument, &series_id) {
                self.subscribe_series(series_id);
            }
        }
        Ok(())
    }

    fn on_option_chain(&mut self, slice: &OptionChainSlice) -> anyhow::Result<()> {
        self.drain_scan_results();
        let trade_date = market_trade_date(self.config.scan.trade_date_timezone);
        let ts_init = self.core.timestamp_ns();
        self.enqueue_scan_job(slice, trade_date, ts_init);
        Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        self.store_underlying_bar(*bar);
        Ok(())
    }

    fn on_historical_bars(&mut self, bars: &[Bar]) -> anyhow::Result<()> {
        if let Some(first) = bars.first() {
            self.store_underlying_bars(first.bar_type, bars.to_vec());
        }
        Ok(())
    }

    fn on_time_event(&mut self, event: &TimeEvent) -> anyhow::Result<()> {
        if event.name.as_str() == SCAN_RESULT_TIMER {
            self.drain_scan_results();
        }
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.clock().cancel_timer(SCAN_RESULT_TIMER);
        self.stop_scan_workers();
        for series_id in self.subscribed_series.iter().copied().collect::<Vec<_>>() {
            self.unsubscribe_option_chain(series_id, self.config.client_id);
        }
        self.subscribed_series.clear();
        self.underlying_bar_types.clear();
        self.latest_underlying_bars.clear();
        Ok(())
    }
}

/// Builds read-only option-chain scanner settings from the Alpaca options runtime config.
#[must_use]
pub fn candidate_scan_config_from_runtime(
    config: &AlpacaOptionsRuntimeConfig,
    options_buying_power: Option<f64>,
) -> OptionChainCandidateScanConfig {
    OptionChainCandidateScanConfig {
        strategy_profiles: config.strategy_profiles.clone(),
        spread_kinds: config.spread_kinds.clone(),
        iron_condor_enabled: config.iron_condor_enabled,
        debit_kinds: config.debit_kinds.clone(),
        naked_kinds: config.naked_kinds.clone(),
        credit_scanner: config.scanner.clone(),
        iron_condor_scanner: config.iron_condor_scanner.clone(),
        debit_scanner: config.debit_scanner.clone(),
        naked_scanner: config.naked_scanner.clone(),
        naked_1_3dte_scanner: config.naked_1_3dte_scanner.clone(),
        options_buying_power,
        quantity: config.quantity,
        candidate_ledger_max_candidates: config.candidate_ledger_max_candidates,
        trade_date_timezone: config.entry_timezone,
        regime_features: RegimeFeatureConfig {
            option_quote_stale_after_secs: config.active_risk_quote_stale_secs,
            ..Default::default()
        },
        underlying_bar_lookback_days: 90,
        underlying_bar_limit: 120,
        event_shock_earnings_events: config.event_shock_earnings_events.clone(),
        event_shock_block_days_before_earnings: config.event_shock_block_days_before_earnings,
        event_shock_block_days_after_earnings: config.event_shock_block_days_after_earnings,
    }
}

fn underlying_daily_bar_type(underlying: &str) -> BarType {
    BarType::new(
        InstrumentId::from(format!("{underlying}.{ALPACA_VENUE}").as_str()),
        BarSpecification::new(1, BarAggregation::Day, PriceType::Last),
        AggregationSource::External,
    )
}

fn candidate_ledger_candidate_limit(max_candidates: usize, candidate_count: usize) -> usize {
    if max_candidates == 0 {
        candidate_count
    } else {
        candidate_count.min(max_candidates)
    }
}

/// Discovers ranked option candidates from one Nautilus option-chain slice.
#[must_use]
pub fn scan_option_chain_candidates(
    slice: &OptionChainSlice,
    config: &OptionChainCandidateScanConfig,
    trade_date: &str,
) -> OptionsCandidateSet {
    let input = option_chain_candidate_input(slice);
    scan_option_chain_candidate_input(
        &input,
        config,
        trade_date,
        scan_date_from_timestamp(slice.ts_event),
    )
}

fn scan_option_chain_candidate_input(
    input: &OptionChainCandidateInput,
    config: &OptionChainCandidateScanConfig,
    trade_date: &str,
    scan_date: NaiveDate,
) -> OptionsCandidateSet {
    let mut candidates = OptionsCandidateSet::new(trade_date);

    if !config.strategy_profiles.is_empty() {
        for profile in config
            .strategy_profiles
            .iter()
            .filter(|profile| profile.scans_underlying(&input.underlying))
        {
            if let (Some(kind), AlpacaOptionsStrategyScannerConfig::Credit(scanner)) =
                (credit_kind_from_family(profile.family), &profile.scanner)
            {
                let result = scan_credit_spread_option_chain(&input, scanner, kind, scan_date);
                let strategy_name = credit_spread_strategy_name(kind);
                candidates.push_scan(OptionsScanReport::new(
                    &input.underlying,
                    strategy_name,
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                if let Some(best) = result.candidates.first() {
                    candidates.consider_candidate(SelectedOptionsEntry::Credit(SelectedEntry {
                        underlying: input.underlying.clone(),
                        kind,
                        candidate: best.clone(),
                    }));
                }
            }

            if matches!(profile.family, AlpacaOptionsStrategyFamily::IronCondor) {
                let AlpacaOptionsStrategyScannerConfig::IronCondor(scanner) = &profile.scanner
                else {
                    continue;
                };
                let result = scan_iron_condor_option_chain(&input, scanner, scan_date);
                candidates.push_scan(OptionsScanReport::new(
                    &input.underlying,
                    "iron_condor",
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                if let Some(best) = result.candidates.first() {
                    candidates.consider_candidate(SelectedOptionsEntry::IronCondor(
                        SelectedIronCondorEntry {
                            underlying: input.underlying.clone(),
                            candidate: best.clone(),
                        },
                    ));
                }
            }

            if let (Some(kind), AlpacaOptionsStrategyScannerConfig::Debit(scanner)) =
                (debit_kind_from_family(profile.family), &profile.scanner)
            {
                let result = scan_debit_spread_option_chain(&input, scanner, kind, scan_date);
                let strategy_name = debit_spread_strategy_name(kind);
                candidates.push_scan(OptionsScanReport::new(
                    &input.underlying,
                    strategy_name,
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                if let Some(best) = result.candidates.first() {
                    candidates.consider_candidate(SelectedOptionsEntry::Debit(
                        SelectedDebitEntry {
                            underlying: input.underlying.clone(),
                            kind,
                            candidate: best.clone(),
                        },
                    ));
                }
            }

            if let (Some(kind), AlpacaOptionsStrategyScannerConfig::Naked(scanner)) =
                (naked_kind_from_family(profile.family), &profile.scanner)
            {
                let result = scan_naked_option_chain(
                    &input,
                    scanner,
                    kind,
                    Some(NakedOptionCapitalContext {
                        options_buying_power: config.options_buying_power,
                        quantity: profile.quantity,
                    }),
                    scan_date,
                );
                let strategy_name = naked_option_strategy_name(kind);
                candidates.push_scan(OptionsScanReport::new(
                    &input.underlying,
                    strategy_name,
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                if let Some(best) = result.candidates.first() {
                    candidates.consider_candidate(SelectedOptionsEntry::NakedOption(
                        SelectedNakedOptionEntry {
                            underlying: input.underlying.clone(),
                            kind,
                            candidate: best.clone(),
                        },
                    ));
                }
            }
        }

        return candidates;
    }

    for kind in &config.spread_kinds {
        let result =
            scan_credit_spread_option_chain(&input, &config.credit_scanner, *kind, scan_date);
        let strategy_name = credit_spread_strategy_name(*kind);
        candidates.push_scan(OptionsScanReport::new(
            &input.underlying,
            strategy_name,
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(SelectedOptionsEntry::Credit(SelectedEntry {
                underlying: input.underlying.clone(),
                kind: *kind,
                candidate: best.clone(),
            }));
        }
    }

    if config.iron_condor_enabled {
        let result = scan_iron_condor_option_chain(&input, &config.iron_condor_scanner, scan_date);
        candidates.push_scan(OptionsScanReport::new(
            &input.underlying,
            "iron_condor",
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(SelectedOptionsEntry::IronCondor(
                SelectedIronCondorEntry {
                    underlying: input.underlying.clone(),
                    candidate: best.clone(),
                },
            ));
        }
    }

    for kind in &config.debit_kinds {
        let result =
            scan_debit_spread_option_chain(&input, &config.debit_scanner, *kind, scan_date);
        let strategy_name = debit_spread_strategy_name(*kind);
        candidates.push_scan(OptionsScanReport::new(
            &input.underlying,
            strategy_name,
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(SelectedOptionsEntry::Debit(SelectedDebitEntry {
                underlying: input.underlying.clone(),
                kind: *kind,
                candidate: best.clone(),
            }));
        }
    }

    for kind in &config.naked_kinds {
        let result = scan_naked_option_chain(
            &input,
            naked_scanner_for(config, *kind),
            *kind,
            Some(NakedOptionCapitalContext {
                options_buying_power: config.options_buying_power,
                quantity: config.quantity,
            }),
            scan_date,
        );
        let strategy_name = naked_option_strategy_name(*kind);
        candidates.push_scan(OptionsScanReport::new(
            &input.underlying,
            strategy_name,
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(SelectedOptionsEntry::NakedOption(
                SelectedNakedOptionEntry {
                    underlying: input.underlying.clone(),
                    kind: *kind,
                    candidate: best.clone(),
                },
            ));
        }
    }

    candidates
}

fn naked_scanner_for(
    config: &OptionChainCandidateScanConfig,
    kind: NakedOptionKind,
) -> &NakedOptionScannerConfig {
    if kind.is_one_to_three_dte() {
        &config.naked_1_3dte_scanner
    } else {
        &config.naked_scanner
    }
}

fn credit_kind_from_family(family: AlpacaOptionsStrategyFamily) -> Option<CreditSpreadKind> {
    match family {
        AlpacaOptionsStrategyFamily::PutCredit => Some(CreditSpreadKind::Put),
        AlpacaOptionsStrategyFamily::CallCredit => Some(CreditSpreadKind::Call),
        _ => None,
    }
}

fn debit_kind_from_family(family: AlpacaOptionsStrategyFamily) -> Option<DebitSpreadKind> {
    match family {
        AlpacaOptionsStrategyFamily::PutDebit => Some(DebitSpreadKind::Put),
        AlpacaOptionsStrategyFamily::CallDebit => Some(DebitSpreadKind::Call),
        _ => None,
    }
}

fn naked_kind_from_family(family: AlpacaOptionsStrategyFamily) -> Option<NakedOptionKind> {
    match family {
        AlpacaOptionsStrategyFamily::NakedPut => Some(NakedOptionKind::Put),
        AlpacaOptionsStrategyFamily::NakedCall => Some(NakedOptionKind::Call),
        AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte => {
            Some(NakedOptionKind::PutOneToThreeDte)
        }
        AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte => {
            Some(NakedOptionKind::CallOneToThreeDte)
        }
        _ => None,
    }
}

fn candidate_event_payload(
    source: &ScanSourceSummary,
    candidates: &OptionsCandidateSet,
    regime_context: &RegimeContext,
    routing_summary: &RegimeRoutingSummary,
) -> Value {
    let mut payload = json!({
        "source": "option_chain",
        "series_id": source.series_id.to_string(),
        "underlying": source.underlying,
        "trade_date": candidates.trade_date,
        "expiration_date": source.expiration_date,
        "underlying_price": source.underlying_price,
        "call_contracts": source.call_contracts,
        "put_contracts": source.put_contracts,
        "source_ts_event": source.source_ts_event.as_u64(),
        "source_ts_init": source.source_ts_init.as_u64(),
        "candidate_ts_init": source.candidate_ts_init.as_u64(),
        "scans": candidates.scans.iter().map(scan_report_payload).collect::<Vec<_>>(),
        "ranked_entries": candidates.ranked_entries().len(),
        "selected": candidates.selected_entry().map(selected_entry_payload),
    });
    insert_regime_context(&mut payload, Some(regime_context));
    if let Value::Object(fields) = &mut payload {
        fields.insert(
            "regime_routing".to_string(),
            routing_summary.to_json_value(),
        );
    }
    payload
}

fn scan_report_payload(report: &OptionsScanReport) -> Value {
    json!({
        "underlying": report.underlying,
        "strategy": report.strategy,
        "outcome": match report.outcome {
            OptionsScanOutcome::Candidate => "candidate",
            OptionsScanOutcome::NoCandidate => "no_candidate",
        },
        "candidate_count": report.candidate_count,
        "reason": report.reason,
        "contracts": report.contract_count,
        "snapshots": report.snapshot_count,
        "scoreable": report.scoreable_count,
        "rejections": report.rejection_counts,
    })
}

fn selected_entry_payload(entry: &SelectedOptionsEntry) -> Value {
    let descriptor = entry.descriptor();
    json!({
        "strategy": descriptor.strategy,
        "underlying": descriptor.underlying,
        "candidate_type": descriptor.candidate_type,
        "symbols": descriptor.symbols,
        "score": descriptor.score,
        "premium_kind": descriptor.premium_kind.as_str(),
        "premium": descriptor.premium,
    })
}

fn market_trade_date(timezone: Tz) -> String {
    Utc::now()
        .with_timezone(&timezone)
        .date_naive()
        .format("%Y-%m-%d")
        .to_string()
}

fn scan_date_from_timestamp(ts_event: UnixNanos) -> NaiveDate {
    let date = ts_event.to_datetime_utc().date_naive();
    if date.year() < 2000 {
        Utc::now().date_naive()
    } else {
        date
    }
}

fn instrument_belongs_to_series(instrument: &InstrumentAny, series_id: &OptionSeriesId) -> bool {
    instrument.venue() == series_id.venue
        && instrument
            .underlying()
            .is_some_and(|value| value == series_id.underlying)
        && instrument.expiration_ns() == Some(series_id.expiration_ns)
        && instrument.settlement_currency().code == series_id.settlement_currency
        && instrument.strike_price().is_some()
        && instrument.option_kind().is_some()
}

#[derive(Debug)]
struct ScanWorkerPool {
    queue: Arc<ScanJobQueue>,
    result_rx: mpsc::Receiver<ScanWorkerMessage>,
    handles: Vec<JoinHandle<()>>,
    queue_capacity: usize,
    worker_count: usize,
}

impl ScanWorkerPool {
    fn start(
        scan_config: OptionChainCandidateScanConfig,
        queue_capacity: usize,
        worker_count: usize,
    ) -> anyhow::Result<Self> {
        let queue = Arc::new(ScanJobQueue::new(queue_capacity));
        let (result_tx, result_rx) = mpsc::channel();
        let mut handles = Vec::with_capacity(worker_count);

        for worker_index in 0..worker_count {
            let worker_queue = Arc::clone(&queue);
            let worker_result_tx = result_tx.clone();
            let worker_scan_config = scan_config.clone();
            let handle = thread::Builder::new()
                .name(format!("alpaca-option-scan-{worker_index}"))
                .spawn(move || {
                    scan_worker_loop(
                        worker_index,
                        worker_queue,
                        worker_result_tx,
                        worker_scan_config,
                    );
                })
                .map_err(|error| {
                    queue.close();
                    anyhow::anyhow!("failed to start Alpaca option-chain scan worker: {error}")
                })?;
            handles.push(handle);
        }
        drop(result_tx);

        Ok(Self {
            queue,
            result_rx,
            handles,
            queue_capacity,
            worker_count,
        })
    }

    fn enqueue(&self, job: ScanJob) -> ScanEnqueueOutcome {
        self.queue.push_latest(job)
    }

    fn try_recv(&self) -> Option<ScanWorkerMessage> {
        self.result_rx.try_recv().ok()
    }

    fn queue_capacity(&self) -> usize {
        self.queue_capacity
    }

    fn worker_count(&self) -> usize {
        self.worker_count
    }

    fn shutdown(mut self) -> Vec<ScanWorkerMessage> {
        self.queue.close();
        for handle in self.handles.drain(..) {
            if let Err(error) = handle.join() {
                log::error!("Alpaca option-chain scan worker thread failed: {error:?}");
            }
        }

        let mut messages = Vec::new();
        while let Ok(message) = self.result_rx.try_recv() {
            messages.push(message);
        }
        messages
    }
}

#[derive(Debug)]
struct ScanJobQueue {
    capacity: usize,
    state: Mutex<ScanJobQueueState>,
    available: Condvar,
}

impl ScanJobQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(ScanJobQueueState {
                jobs: VecDeque::new(),
                closed: false,
            }),
            available: Condvar::new(),
        }
    }

    fn push_latest(&self, job: ScanJob) -> ScanEnqueueOutcome {
        let mut state = self.lock_state();
        if state.closed {
            return ScanEnqueueOutcome::Closed;
        }

        let dropped_oldest = if state.jobs.len() >= self.capacity {
            state.jobs.pop_front().map(|dropped| dropped.summary())
        } else {
            None
        };
        state.jobs.push_back(job);
        let depth = state.jobs.len();
        self.available.notify_one();
        ScanEnqueueOutcome::Enqueued {
            depth,
            dropped_oldest,
        }
    }

    fn recv(&self) -> Option<ScanJob> {
        let mut state = self.lock_state();
        loop {
            if let Some(job) = state.jobs.pop_front() {
                return Some(job);
            }
            if state.closed {
                return None;
            }
            state = match self.available.wait(state) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    fn close(&self) {
        let mut state = self.lock_state();
        state.closed = true;
        self.available.notify_all();
    }

    fn lock_state(&self) -> MutexGuard<'_, ScanJobQueueState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[derive(Debug)]
struct ScanJobQueueState {
    jobs: VecDeque<ScanJob>,
    closed: bool,
}

#[derive(Debug)]
enum ScanEnqueueOutcome {
    Enqueued {
        depth: usize,
        dropped_oldest: Option<ScanJobSummary>,
    },
    Closed,
}

#[derive(Debug)]
struct ScanJob {
    sequence: u64,
    slice: OptionChainSlice,
    trade_date: String,
    ts_init: UnixNanos,
    underlying_bars: Vec<Bar>,
    enqueued_at: Instant,
}

impl ScanJob {
    fn summary(&self) -> ScanJobSummary {
        ScanJobSummary {
            sequence: self.sequence,
            series_id: self.slice.series_id.to_string(),
            source_ts_event: self.slice.ts_event,
            source_ts_init: self.slice.ts_init,
        }
    }
}

#[derive(Clone, Debug)]
struct ScanJobSummary {
    sequence: u64,
    series_id: String,
    source_ts_event: UnixNanos,
    source_ts_init: UnixNanos,
}

#[derive(Debug)]
enum ScanWorkerMessage {
    Result(ScanWorkerResult),
    Error(ScanWorkerError),
}

#[derive(Debug)]
struct ScanWorkerError {
    worker_index: usize,
    source: ScanJobSummary,
    reason: &'static str,
    enqueued_at: Instant,
    started_at: Instant,
    completed_at: Instant,
}

#[derive(Debug)]
struct ScanWorkerResult {
    worker_index: usize,
    source: ScanSourceSummary,
    candidates: OptionsCandidateSet,
    feature_snapshot: RegimeFeatureSnapshot,
    regime_context: RegimeContext,
    enqueued_at: Instant,
    started_at: Instant,
    completed_at: Instant,
}

#[derive(Debug)]
struct ScanSourceSummary {
    sequence: u64,
    series_id: OptionSeriesId,
    source_ts_event: UnixNanos,
    source_ts_init: UnixNanos,
    candidate_ts_init: UnixNanos,
    trade_date: String,
    underlying: String,
    expiration_date: String,
    underlying_price: Option<f64>,
    call_contracts: usize,
    put_contracts: usize,
}

fn scan_worker_loop(
    worker_index: usize,
    queue: Arc<ScanJobQueue>,
    result_tx: mpsc::Sender<ScanWorkerMessage>,
    scan_config: OptionChainCandidateScanConfig,
) {
    while let Some(job) = queue.recv() {
        let started_at = Instant::now();
        let summary = job.summary();
        let enqueued_at = job.enqueued_at;
        let result = catch_unwind(AssertUnwindSafe(|| {
            scan_worker_job(worker_index, job, &scan_config, started_at)
        }));
        let message = match result {
            Ok(result) => ScanWorkerMessage::Result(result),
            Err(_) => ScanWorkerMessage::Error(ScanWorkerError {
                worker_index,
                source: summary,
                reason: "scan_worker_panic",
                enqueued_at,
                started_at,
                completed_at: Instant::now(),
            }),
        };

        if result_tx.send(message).is_err() {
            break;
        }
    }
}

fn scan_worker_job(
    worker_index: usize,
    job: ScanJob,
    config: &OptionChainCandidateScanConfig,
    started_at: Instant,
) -> ScanWorkerResult {
    let event_load_events = config
        .event_shock_earnings_events
        .iter()
        .map(|event| {
            RegimeEvent::new(
                event.underlying.clone(),
                event.report_date,
                event.source.clone(),
            )
        })
        .collect::<Vec<_>>();
    let feature_inputs = RegimeFeatureInputs {
        underlying_bars: &job.underlying_bars,
        event_load_events: &event_load_events,
        event_load_block_days_before: config.event_shock_block_days_before_earnings,
        event_load_block_days_after: config.event_shock_block_days_after_earnings,
    };
    let feature_snapshot = regime_feature_snapshot_from_option_chain(
        &job.slice,
        &config.regime_features,
        feature_inputs,
        &job.trade_date,
        job.ts_init,
    );
    let regime_context = regime_context_from_features(&feature_snapshot);
    let input = option_chain_candidate_input(&job.slice);
    let candidates = scan_option_chain_candidate_input(
        &input,
        config,
        &job.trade_date,
        scan_date_from_timestamp(job.slice.ts_event),
    );
    let source = ScanSourceSummary {
        sequence: job.sequence,
        series_id: job.slice.series_id,
        source_ts_event: job.slice.ts_event,
        source_ts_init: job.slice.ts_init,
        candidate_ts_init: job.ts_init,
        trade_date: job.trade_date,
        underlying: input.underlying,
        expiration_date: input.expiration_date,
        underlying_price: input.underlying_price,
        call_contracts: input.calls.contract_count(),
        put_contracts: input.puts.contract_count(),
    };

    ScanWorkerResult {
        worker_index,
        source,
        candidates,
        feature_snapshot,
        regime_context,
        enqueued_at: job.enqueued_at,
        started_at,
        completed_at: Instant::now(),
    }
}

fn duration_ms_u64(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
