//! Read-only Nautilus actor for option-chain candidate evidence.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
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
    messages::data::InstrumentsResponse,
    nautilus_actor,
    timer::TimeEvent,
};
use nautilus_core::{Params, UUID4, UnixNanos};
use nautilus_model::{
    data::{
        Bar, BarSpecification, BarType,
        option_chain::{OptionChainSlice, StrikeRange},
    },
    enums::{AggregationSource, BarAggregation, PriceType},
    identifiers::{ActorId, ClientId, InstrumentId, OptionSeriesId, Venue},
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
        RegimeFeatureSnapshot, RegimeRoutingSummary, insert_regime_context,
        regime_context_from_features, regime_feature_snapshot_from_option_chain,
    },
    universe::{
        OptionDteWindow, OptionUniverseContract, OptionUniverseIntent, OptionUniverseRequiredSides,
        OptionUniverseResolution, OptionUniverseStrategyFamily, ResolvedOptionUniverseSeries,
        SkippedOptionUniverseIntent, option_series_matches_intent_dte, resolve_option_universe,
    },
};
use serde_json::{Value, json};

use crate::{
    candidate_ledger_persistence::CandidateLedgerPersistenceHandle,
    candidate_payloads::selected_entry_candidate_ledger_payload,
    common::consts::{
        ALPACA_OPTION_CHAIN_EXPIRATION_PARAM, ALPACA_OPTION_CHAIN_MAX_EXPIRATION_PARAM,
        ALPACA_OPTION_CHAIN_MIN_EXPIRATION_PARAM, ALPACA_OPTION_CHAIN_TYPE_PARAM,
        ALPACA_OPTION_CHAIN_UNDERLYING_PARAM, ALPACA_OPTION_QUOTE_INTEREST_CHAIN_SCAN,
        ALPACA_OPTION_QUOTE_INTEREST_PARAM, ALPACA_OPTION_QUOTE_STREAM_POLICY_PARAM,
        ALPACA_OPTION_QUOTE_STREAM_POLICY_SNAPSHOT_ONLY, ALPACA_VENUE,
    },
    earnings::EarningsEvent,
    option_chain_candidates::{
        OptionChainCandidateInput, option_chain_candidate_input, scan_credit_spread_option_chain,
        scan_debit_spread_option_chain, scan_iron_condor_option_chain, scan_naked_option_chain,
    },
    options_account_strategy::OptionsCandidateData,
    options_runtime::{
        AlpacaOptionsCandidateProfile, AlpacaOptionsRuntimeConfig, AlpacaOptionsStrategyFamily,
        AlpacaOptionsStrategyProfile, AlpacaOptionsStrategyScannerConfig, OptionsCandidateSet,
        OptionsScanOutcome, OptionsScanReport, ProfiledOptionsEntry,
    },
    runtime::emit_operator_event,
};

const SCAN_RESULT_TIMER: &str = "alpaca_option_chain_scan_results";
const UNIVERSE_REFRESH_TIMER: &str = "alpaca_option_universe_refresh";
const DEFAULT_SCAN_QUEUE_CAPACITY: usize = 4;
const DEFAULT_SCAN_WORKER_THREADS: usize = 2;
const DEFAULT_SCAN_RESULT_DRAIN_INTERVAL_MS: u64 = 250;
const DEFAULT_SCAN_MAX_RESULT_AGE_MS: u64 = 15_000;
const DEFAULT_UNIVERSE_REFRESH_INTERVAL_SECS: u64 = 300;

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
    /// Explicit option series subscriptions for catalog, backtest, and diagnostics.
    pub series: Vec<OptionSeriesId>,
    /// Strategy-derived universe intents for live dynamic resolution.
    pub universe_intents: Vec<OptionUniverseIntent>,
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
    /// Interval in seconds for dynamic universe rollover/stale-series checks.
    pub universe_refresh_interval_secs: u64,
}

impl Default for OptionChainCandidateScanActorConfig {
    fn default() -> Self {
        Self {
            actor_id: Some(ActorId::from("ALPACA-OPPORTUNITY-SCAN")),
            series: Vec::new(),
            universe_intents: Vec::new(),
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
            universe_refresh_interval_secs: DEFAULT_UNIVERSE_REFRESH_INTERVAL_SECS,
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
    pending_universe_intents: HashMap<UUID4, PendingUniverseRequest>,
    selected_series_by_profile_underlying: BTreeMap<UniverseIntentKey, OptionSeriesId>,
    selected_profiles_by_series: BTreeMap<OptionSeriesId, Vec<AlpacaOptionsStrategyProfile>>,
    last_universe_trade_date: Option<NaiveDate>,
    underlying_bar_types: BTreeMap<String, BarType>,
    latest_underlying_bars: BTreeMap<String, Vec<Bar>>,
    latest_candidates: Option<OptionsCandidateSet>,
    scan_workers: Option<ScanWorkerPool>,
    latest_enqueued_scan_sequence: u64,
    latest_published_scan_sequence: u64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct UniverseIntentKey {
    profile_id: String,
    underlying: String,
}

impl UniverseIntentKey {
    fn new(profile_id: impl Into<String>, underlying: impl Into<String>) -> Self {
        Self {
            profile_id: profile_id.into(),
            underlying: underlying.into(),
        }
    }
}

#[derive(Clone, Debug)]
struct PendingUniverseRequest {
    reason: String,
    intents: Vec<OptionUniverseIntent>,
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
            pending_universe_intents: HashMap::new(),
            selected_series_by_profile_underlying: BTreeMap::new(),
            selected_profiles_by_series: BTreeMap::new(),
            last_universe_trade_date: None,
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

    fn subscribe_series(&mut self, series_id: OptionSeriesId) -> bool {
        if !self.subscribed_series.insert(series_id) {
            return false;
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
            Some(option_chain_quote_params()),
        );
        true
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

    fn request_universe_intent_instruments(
        &mut self,
        intent: OptionUniverseIntent,
        reason: &str,
    ) -> anyhow::Result<()> {
        let trade_date = market_trade_naive_date(self.config.scan.trade_date_timezone);
        let min_expiration = trade_date + Duration::days(intent.dte_window.min_dte);
        let max_expiration = trade_date + Duration::days(intent.dte_window.max_dte);

        let mut params = Params::new();
        params.insert(
            ALPACA_OPTION_CHAIN_UNDERLYING_PARAM.to_string(),
            json!(intent.underlying.as_str()),
        );
        params.insert(
            ALPACA_OPTION_CHAIN_MIN_EXPIRATION_PARAM.to_string(),
            json!(min_expiration.format("%Y-%m-%d").to_string()),
        );
        params.insert(
            ALPACA_OPTION_CHAIN_MAX_EXPIRATION_PARAM.to_string(),
            json!(max_expiration.format("%Y-%m-%d").to_string()),
        );
        if let Some(option_type) = option_type_param_for_required_sides(intent.required_sides) {
            params.insert(
                ALPACA_OPTION_CHAIN_TYPE_PARAM.to_string(),
                json!(option_type),
            );
        }

        log::info!(
            "Requesting Alpaca option instruments for universe intent profile={} underlying={} family={} dte={}..{} sides={} reason={}",
            intent.profile_id,
            intent.underlying,
            intent.strategy_family.as_str(),
            intent.dte_window.min_dte,
            intent.dte_window.max_dte,
            intent.required_sides.as_str(),
            reason,
        );
        let request_id = self.request_instruments(
            Some(Venue::from(ALPACA_VENUE)),
            None,
            None,
            self.config.client_id,
            Some(params),
        )?;
        self.pending_universe_intents.insert(
            request_id,
            PendingUniverseRequest {
                reason: reason.to_string(),
                intents: vec![intent],
            },
        );
        Ok(())
    }

    fn handle_universe_instruments_response(
        &mut self,
        response: &nautilus_common::messages::data::InstrumentsResponse,
    ) -> anyhow::Result<bool> {
        let Some(pending) = self
            .pending_universe_intents
            .remove(&response.correlation_id)
        else {
            return Ok(false);
        };
        let contracts = response
            .data
            .iter()
            .filter_map(option_universe_contract_from_instrument)
            .collect::<Vec<_>>();
        let resolution =
            resolve_option_universe(&pending.intents, &contracts, &[], self.core.timestamp_ns());
        self.apply_universe_resolution(&pending.reason, &resolution)?;
        self.emit_universe_resolution(&pending.reason, &resolution);
        Ok(true)
    }

    fn apply_universe_resolution(
        &mut self,
        reason: &str,
        resolution: &OptionUniverseResolution,
    ) -> anyhow::Result<()> {
        for selected in &resolution.selected {
            let series_id = selected.coverage.series_id;
            let key = UniverseIntentKey::new(&selected.profile_id, &selected.underlying);
            let previous = self.selected_series_by_profile_underlying.remove(&key);
            match previous {
                Some(previous_series_id) if previous_series_id != series_id => {
                    self.remove_profile_from_selected_series(previous_series_id, &key, reason);
                    self.emit_universe_rollover_selected(
                        reason,
                        selected,
                        Some(previous_series_id),
                    );
                }
                None if reason != "startup" => {
                    self.emit_universe_rollover_selected(reason, selected, None);
                }
                _ => {}
            }
            self.selected_series_by_profile_underlying
                .insert(key.clone(), series_id);
            self.add_profile_to_selected_series(series_id, &key);
            if self.subscribe_series(series_id) {
                emit_operator_event(
                    "option_universe_subscription",
                    json!({
                        "event": "subscribed",
                        "reason": reason,
                        "profile_id": selected.profile_id,
                        "underlying": selected.underlying,
                        "series_id": series_id.to_string(),
                        "dte": selected.coverage.dte,
                    }),
                );
            }
            if let Err(error) = self.request_underlying_bars(series_id) {
                log::warn!("Failed to request Alpaca underlying bars for {series_id}: {error:#}");
            }
        }
        for skipped in &resolution.skipped {
            let key = UniverseIntentKey::new(&skipped.profile_id, &skipped.underlying);
            if let Some(previous_series_id) =
                self.selected_series_by_profile_underlying.remove(&key)
            {
                self.remove_profile_from_selected_series(previous_series_id, &key, reason);
                self.emit_universe_rollover_skipped(reason, skipped, previous_series_id);
            }
        }
        Ok(())
    }

    fn add_profile_to_selected_series(
        &mut self,
        series_id: OptionSeriesId,
        key: &UniverseIntentKey,
    ) {
        let Some(profile) = self.profile_for_intent(key) else {
            return;
        };
        let profiles = self
            .selected_profiles_by_series
            .entry(series_id)
            .or_default();
        if !profiles
            .iter()
            .any(|existing| existing.id == profile.id && existing.scans_underlying(&key.underlying))
        {
            profiles.push(profile);
        }
    }

    fn remove_profile_from_selected_series(
        &mut self,
        series_id: OptionSeriesId,
        key: &UniverseIntentKey,
        reason: &str,
    ) {
        let remove_series =
            if let Some(profiles) = self.selected_profiles_by_series.get_mut(&series_id) {
                profiles.retain(|profile| {
                    !(profile.id == key.profile_id && profile.scans_underlying(&key.underlying))
                });
                profiles.is_empty()
            } else {
                true
            };
        if remove_series {
            self.selected_profiles_by_series.remove(&series_id);
            self.unsubscribe_dynamic_series_if_unused(series_id, reason);
        }
    }

    fn unsubscribe_dynamic_series_if_unused(&mut self, series_id: OptionSeriesId, reason: &str) {
        if self
            .selected_series_by_profile_underlying
            .values()
            .any(|selected_series_id| *selected_series_id == series_id)
        {
            return;
        }
        if self.subscribed_series.remove(&series_id) {
            self.unsubscribe_option_chain(series_id, self.config.client_id);
            emit_operator_event(
                "option_universe_subscription",
                json!({
                    "event": "unsubscribed_stale",
                    "reason": reason,
                    "series_id": series_id.to_string(),
                }),
            );
        }
    }

    fn emit_universe_resolution(&self, reason: &str, resolution: &OptionUniverseResolution) {
        let selected_series = resolution
            .selected
            .iter()
            .map(|selected| selected.coverage.series_id.to_string())
            .collect::<Vec<_>>();
        let mut skipped_reasons = BTreeMap::<String, usize>::new();
        for skipped in &resolution.skipped {
            *skipped_reasons
                .entry(skipped.reason.as_str().to_string())
                .or_default() += 1;
        }
        let resolution_payload = serde_json::to_value(resolution).unwrap_or_else(|error| {
            json!({
                "serialization_error": error.to_string(),
                "selected_count": resolution.selected.len(),
                "skipped_count": resolution.skipped.len(),
            })
        });
        emit_operator_event(
            "option_universe_resolution",
            json!({
                "reason": reason,
                "requested_count": resolution.selected.len() + resolution.skipped.len(),
                "selected_count": resolution.selected.len(),
                "skipped_count": resolution.skipped.len(),
                "selected_series": selected_series,
                "skipped_reasons": skipped_reasons,
                "resolution": resolution_payload,
            }),
        );
    }

    fn emit_universe_rollover_selected(
        &self,
        reason: &str,
        selected: &ResolvedOptionUniverseSeries,
        previous_series_id: Option<OptionSeriesId>,
    ) {
        emit_operator_event(
            "option_universe_rollover",
            json!({
                "event": "selected",
                "reason": reason,
                "profile_id": selected.profile_id,
                "underlying": selected.underlying,
                "previous_series_id": previous_series_id.map(|series_id| series_id.to_string()),
                "selected_series_id": selected.coverage.series_id.to_string(),
                "selected_dte": selected.coverage.dte,
                "selection_reason": selected.reason.as_str(),
            }),
        );
    }

    fn emit_universe_rollover_skipped(
        &self,
        reason: &str,
        skipped: &SkippedOptionUniverseIntent,
        previous_series_id: OptionSeriesId,
    ) {
        emit_operator_event(
            "option_universe_rollover",
            json!({
                "event": "skipped",
                "reason": reason,
                "profile_id": skipped.profile_id,
                "underlying": skipped.underlying,
                "previous_series_id": previous_series_id.to_string(),
                "skip_reason": skipped.reason.as_str(),
            }),
        );
    }

    fn profile_for_intent(&self, key: &UniverseIntentKey) -> Option<AlpacaOptionsStrategyProfile> {
        self.config
            .scan
            .strategy_profiles
            .iter()
            .find(|profile| {
                profile.id == key.profile_id && profile.scans_underlying(&key.underlying)
            })
            .cloned()
    }

    fn has_dynamic_universe(&self) -> bool {
        !self.config.universe_intents.is_empty()
    }

    fn request_dynamic_universe_resolution(&mut self, reason: &str) -> anyhow::Result<()> {
        if !self.pending_universe_intents.is_empty() {
            emit_operator_event(
                "option_universe_refresh",
                json!({
                    "event": "skipped",
                    "reason": reason,
                    "skip_reason": "pending_requests",
                    "pending_requests": self.pending_universe_intents.len(),
                }),
            );
            return Ok(());
        }

        self.last_universe_trade_date = Some(market_trade_naive_date(
            self.config.scan.trade_date_timezone,
        ));
        emit_operator_event(
            "option_universe_refresh",
            json!({
                "event": "requested",
                "reason": reason,
                "intent_count": self.config.universe_intents.len(),
                "trade_date": self
                    .last_universe_trade_date
                    .map(|date| date.format("%Y-%m-%d").to_string()),
            }),
        );
        for intent in self.config.universe_intents.clone() {
            self.request_universe_intent_instruments(intent, reason)?;
        }
        Ok(())
    }

    fn refresh_dynamic_universe_if_needed(&mut self) -> anyhow::Result<()> {
        let Some(reason) = self.dynamic_universe_refresh_reason() else {
            return Ok(());
        };
        self.request_dynamic_universe_resolution(reason)
    }

    fn dynamic_universe_refresh_reason(&mut self) -> Option<&'static str> {
        if !self.has_dynamic_universe() {
            return None;
        }
        let trade_date = market_trade_naive_date(self.config.scan.trade_date_timezone);
        if self
            .last_universe_trade_date
            .is_some_and(|last_trade_date| last_trade_date != trade_date)
        {
            return Some("trade_date_rollover");
        }
        if self.has_stale_selected_universe_series() {
            return Some("stale_dte");
        }
        None
    }

    fn has_stale_selected_universe_series(&self) -> bool {
        let evaluation_time = self.core.timestamp_ns();
        self.selected_series_by_profile_underlying
            .iter()
            .any(|(key, series_id)| {
                let Some(intent) = self.intent_for_key(key) else {
                    return true;
                };
                !option_series_matches_intent_dte(intent, *series_id, evaluation_time)
            })
    }

    fn intent_for_key(&self, key: &UniverseIntentKey) -> Option<&OptionUniverseIntent> {
        self.config.universe_intents.iter().find(|intent| {
            intent.profile_id == key.profile_id && intent.underlying == key.underlying
        })
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
                entry.selected_entry(),
                self.config.scan.options_buying_power,
                Some(index + 1),
            );
            entry.insert_profile_json_fields(&mut payload);
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

        let strategy_profiles = if self.has_dynamic_universe() {
            let Some(profiles) = self.selected_profiles_by_series.get(&slice.series_id) else {
                emit_operator_event(
                    "option_chain_scan_skipped",
                    json!({
                        "reason": "unresolved_universe_series",
                        "series_id": slice.series_id.to_string(),
                        "source_ts_event": slice.ts_event.as_u64(),
                        "source_ts_init": slice.ts_init.as_u64(),
                    }),
                );
                return;
            };
            if profiles.is_empty() {
                emit_operator_event(
                    "option_chain_scan_skipped",
                    json!({
                        "reason": "empty_universe_profile_set",
                        "series_id": slice.series_id.to_string(),
                        "source_ts_event": slice.ts_event.as_u64(),
                        "source_ts_init": slice.ts_init.as_u64(),
                    }),
                );
                return;
            }
            Some(profiles.clone())
        } else {
            None
        };

        self.latest_enqueued_scan_sequence = self.latest_enqueued_scan_sequence.saturating_add(1);
        let underlying = slice.series_id.underlying.to_string();
        let job = ScanJob {
            sequence: self.latest_enqueued_scan_sequence,
            slice: slice.clone(),
            trade_date,
            ts_init,
            strategy_profiles,
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
            apply_profiled_regime_routing(&mut candidates, &result.regime_context);
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
        if self.config.series.is_empty() && self.config.universe_intents.is_empty() {
            log::warn!(
                "Option-chain candidate scan actor has no series subscriptions or universe intents"
            );
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

        if self.has_dynamic_universe() {
            self.clock().set_timer(
                UNIVERSE_REFRESH_TIMER,
                StdDuration::from_secs(self.config.universe_refresh_interval_secs.max(1)),
                None,
                None,
                None,
                None,
                None,
            )?;
            self.request_dynamic_universe_resolution("startup")?;
        } else {
            for series_id in self.config.series.clone() {
                if self.config.bootstrap_instruments {
                    self.request_series_instruments(series_id)?;
                } else {
                    self.subscribe_series(series_id);
                }
                if let Err(error) = self.request_underlying_bars(series_id) {
                    log::warn!(
                        "Failed to request Alpaca underlying bars for {series_id}: {error:#}"
                    );
                }
            }
        }
        Ok(())
    }

    fn on_instruments_response(&mut self, response: &InstrumentsResponse) -> anyhow::Result<()> {
        if self.handle_universe_instruments_response(response)? {
            return Ok(());
        }

        for instrument in &response.data {
            if let Err(error) = self.on_instrument(instrument) {
                log::error!("Error handling Alpaca instruments response instrument: {error:#}");
            }
        }
        Ok(())
    }

    fn on_instrument(&mut self, instrument: &InstrumentAny) -> anyhow::Result<()> {
        if !self.config.bootstrap_instruments || self.has_dynamic_universe() {
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
        match event.name.as_str() {
            SCAN_RESULT_TIMER => self.drain_scan_results(),
            UNIVERSE_REFRESH_TIMER => self.refresh_dynamic_universe_if_needed()?,
            _ => {}
        }
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.clock().cancel_timer(SCAN_RESULT_TIMER);
        self.clock().cancel_timer(UNIVERSE_REFRESH_TIMER);
        self.stop_scan_workers();
        for series_id in self.subscribed_series.iter().copied().collect::<Vec<_>>() {
            self.unsubscribe_option_chain(series_id, self.config.client_id);
        }
        self.subscribed_series.clear();
        self.pending_universe_intents.clear();
        self.selected_series_by_profile_underlying.clear();
        self.selected_profiles_by_series.clear();
        self.last_universe_trade_date = None;
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

/// Builds source-neutral universe intents from resolved Alpaca strategy profiles.
#[must_use]
pub fn option_universe_intents_from_strategy_profiles(
    profiles: &[AlpacaOptionsStrategyProfile],
) -> Vec<OptionUniverseIntent> {
    profiles
        .iter()
        .flat_map(|profile| {
            profile.underlyings.iter().map(|underlying| {
                OptionUniverseIntent::from_family(
                    profile.id.clone(),
                    underlying.clone(),
                    option_universe_strategy_family(profile.family),
                    profile_universe_dte_window(profile),
                )
            })
        })
        .collect()
}

fn option_universe_strategy_family(
    family: AlpacaOptionsStrategyFamily,
) -> OptionUniverseStrategyFamily {
    match family {
        AlpacaOptionsStrategyFamily::PutCredit => OptionUniverseStrategyFamily::PutCredit,
        AlpacaOptionsStrategyFamily::CallCredit => OptionUniverseStrategyFamily::CallCredit,
        AlpacaOptionsStrategyFamily::IronCondor => OptionUniverseStrategyFamily::IronCondor,
        AlpacaOptionsStrategyFamily::PutDebit => OptionUniverseStrategyFamily::PutDebit,
        AlpacaOptionsStrategyFamily::CallDebit => OptionUniverseStrategyFamily::CallDebit,
        AlpacaOptionsStrategyFamily::NakedPut => OptionUniverseStrategyFamily::NakedPut,
        AlpacaOptionsStrategyFamily::NakedCall => OptionUniverseStrategyFamily::NakedCall,
        AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte => {
            OptionUniverseStrategyFamily::NakedPutOneToThreeDte
        }
        AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte => {
            OptionUniverseStrategyFamily::NakedCallOneToThreeDte
        }
    }
}

fn profile_universe_dte_window(profile: &AlpacaOptionsStrategyProfile) -> OptionDteWindow {
    match &profile.scanner {
        AlpacaOptionsStrategyScannerConfig::Credit(scanner) => {
            OptionDteWindow::new(scanner.min_dte, scanner.max_dte)
        }
        AlpacaOptionsStrategyScannerConfig::IronCondor(scanner) => {
            OptionDteWindow::new(scanner.credit.min_dte, scanner.credit.max_dte)
        }
        AlpacaOptionsStrategyScannerConfig::Debit(scanner) => {
            OptionDteWindow::new(scanner.min_dte, scanner.max_dte)
        }
        AlpacaOptionsStrategyScannerConfig::Naked(scanner) => {
            OptionDteWindow::new(scanner.min_dte, scanner.max_dte)
        }
    }
}

fn option_chain_quote_params() -> Params {
    let mut params = Params::new();
    params.insert(
        ALPACA_OPTION_QUOTE_INTEREST_PARAM.to_string(),
        json!(ALPACA_OPTION_QUOTE_INTEREST_CHAIN_SCAN),
    );
    params.insert(
        ALPACA_OPTION_QUOTE_STREAM_POLICY_PARAM.to_string(),
        json!(ALPACA_OPTION_QUOTE_STREAM_POLICY_SNAPSHOT_ONLY),
    );
    params
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
            let profile_context = AlpacaOptionsCandidateProfile::from_strategy_profile(profile);
            if let (Some(kind), AlpacaOptionsStrategyScannerConfig::Credit(scanner)) =
                (credit_kind_from_family(profile.family), &profile.scanner)
            {
                let result = scan_credit_spread_option_chain(&input, scanner, kind, scan_date);
                let strategy_name = credit_spread_strategy_name(kind);
                candidates.push_scan(OptionsScanReport::new(
                    profile_context.clone(),
                    &input.underlying,
                    strategy_name,
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                if let Some(best) = result.candidates.first() {
                    candidates.consider_candidate(ProfiledOptionsEntry::new(
                        profile_context.clone(),
                        SelectedOptionsEntry::Credit(SelectedEntry {
                            underlying: input.underlying.clone(),
                            kind,
                            candidate: best.clone(),
                        }),
                    ));
                }
            }

            if matches!(profile.family, AlpacaOptionsStrategyFamily::IronCondor) {
                let AlpacaOptionsStrategyScannerConfig::IronCondor(scanner) = &profile.scanner
                else {
                    continue;
                };
                let result = scan_iron_condor_option_chain(&input, scanner, scan_date);
                candidates.push_scan(OptionsScanReport::new(
                    profile_context.clone(),
                    &input.underlying,
                    "iron_condor",
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                if let Some(best) = result.candidates.first() {
                    candidates.consider_candidate(ProfiledOptionsEntry::new(
                        profile_context.clone(),
                        SelectedOptionsEntry::IronCondor(SelectedIronCondorEntry {
                            underlying: input.underlying.clone(),
                            candidate: best.clone(),
                        }),
                    ));
                }
            }

            if let (Some(kind), AlpacaOptionsStrategyScannerConfig::Debit(scanner)) =
                (debit_kind_from_family(profile.family), &profile.scanner)
            {
                let result = scan_debit_spread_option_chain(&input, scanner, kind, scan_date);
                let strategy_name = debit_spread_strategy_name(kind);
                candidates.push_scan(OptionsScanReport::new(
                    profile_context.clone(),
                    &input.underlying,
                    strategy_name,
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                if let Some(best) = result.candidates.first() {
                    candidates.consider_candidate(ProfiledOptionsEntry::new(
                        profile_context.clone(),
                        SelectedOptionsEntry::Debit(SelectedDebitEntry {
                            underlying: input.underlying.clone(),
                            kind,
                            candidate: best.clone(),
                        }),
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
                    profile_context.clone(),
                    &input.underlying,
                    strategy_name,
                    result.candidates.len(),
                    result.contract_count,
                    result.snapshot_count,
                    result.scoreable_count,
                    result.rejection_counts.clone(),
                ));
                if let Some(best) = result.candidates.first() {
                    candidates.consider_candidate(ProfiledOptionsEntry::new(
                        profile_context.clone(),
                        SelectedOptionsEntry::NakedOption(SelectedNakedOptionEntry {
                            underlying: input.underlying.clone(),
                            kind,
                            candidate: best.clone(),
                        }),
                    ));
                }
            }
        }

        return candidates;
    }

    for kind in &config.spread_kinds {
        let profile_context = diagnostic_credit_profile_context(*kind, config.quantity);
        let result =
            scan_credit_spread_option_chain(&input, &config.credit_scanner, *kind, scan_date);
        let strategy_name = credit_spread_strategy_name(*kind);
        candidates.push_scan(OptionsScanReport::new(
            profile_context.clone(),
            &input.underlying,
            strategy_name,
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(ProfiledOptionsEntry::new(
                profile_context,
                SelectedOptionsEntry::Credit(SelectedEntry {
                    underlying: input.underlying.clone(),
                    kind: *kind,
                    candidate: best.clone(),
                }),
            ));
        }
    }

    if config.iron_condor_enabled {
        let profile_context = diagnostic_profile_context(
            "diagnostic_iron_condor",
            AlpacaOptionsStrategyFamily::IronCondor,
            config.quantity,
        );
        let result = scan_iron_condor_option_chain(&input, &config.iron_condor_scanner, scan_date);
        candidates.push_scan(OptionsScanReport::new(
            profile_context.clone(),
            &input.underlying,
            "iron_condor",
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(ProfiledOptionsEntry::new(
                profile_context,
                SelectedOptionsEntry::IronCondor(SelectedIronCondorEntry {
                    underlying: input.underlying.clone(),
                    candidate: best.clone(),
                }),
            ));
        }
    }

    for kind in &config.debit_kinds {
        let profile_context = diagnostic_debit_profile_context(*kind, config.quantity);
        let result =
            scan_debit_spread_option_chain(&input, &config.debit_scanner, *kind, scan_date);
        let strategy_name = debit_spread_strategy_name(*kind);
        candidates.push_scan(OptionsScanReport::new(
            profile_context.clone(),
            &input.underlying,
            strategy_name,
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(ProfiledOptionsEntry::new(
                profile_context,
                SelectedOptionsEntry::Debit(SelectedDebitEntry {
                    underlying: input.underlying.clone(),
                    kind: *kind,
                    candidate: best.clone(),
                }),
            ));
        }
    }

    for kind in &config.naked_kinds {
        let profile_context = diagnostic_naked_profile_context(*kind, config.quantity);
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
            profile_context.clone(),
            &input.underlying,
            strategy_name,
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            candidates.consider_candidate(ProfiledOptionsEntry::new(
                profile_context,
                SelectedOptionsEntry::NakedOption(SelectedNakedOptionEntry {
                    underlying: input.underlying.clone(),
                    kind: *kind,
                    candidate: best.clone(),
                }),
            ));
        }
    }

    candidates
}

fn diagnostic_credit_profile_context(
    kind: CreditSpreadKind,
    quantity: u64,
) -> AlpacaOptionsCandidateProfile {
    let family = match kind {
        CreditSpreadKind::Put => AlpacaOptionsStrategyFamily::PutCredit,
        CreditSpreadKind::Call => AlpacaOptionsStrategyFamily::CallCredit,
    };
    diagnostic_profile_context(
        format!("diagnostic_{}", credit_spread_strategy_name(kind)),
        family,
        quantity,
    )
}

fn diagnostic_debit_profile_context(
    kind: DebitSpreadKind,
    quantity: u64,
) -> AlpacaOptionsCandidateProfile {
    let family = match kind {
        DebitSpreadKind::Put => AlpacaOptionsStrategyFamily::PutDebit,
        DebitSpreadKind::Call => AlpacaOptionsStrategyFamily::CallDebit,
    };
    diagnostic_profile_context(
        format!("diagnostic_{}", debit_spread_strategy_name(kind)),
        family,
        quantity,
    )
}

fn diagnostic_naked_profile_context(
    kind: NakedOptionKind,
    quantity: u64,
) -> AlpacaOptionsCandidateProfile {
    let family = match kind {
        NakedOptionKind::Put => AlpacaOptionsStrategyFamily::NakedPut,
        NakedOptionKind::Call => AlpacaOptionsStrategyFamily::NakedCall,
        NakedOptionKind::PutOneToThreeDte => AlpacaOptionsStrategyFamily::NakedPutOneToThreeDte,
        NakedOptionKind::CallOneToThreeDte => AlpacaOptionsStrategyFamily::NakedCallOneToThreeDte,
    };
    diagnostic_profile_context(
        format!("diagnostic_{}", naked_option_strategy_name(kind)),
        family,
        quantity,
    )
}

fn diagnostic_profile_context(
    id: impl Into<String>,
    family: AlpacaOptionsStrategyFamily,
    quantity: u64,
) -> AlpacaOptionsCandidateProfile {
    AlpacaOptionsCandidateProfile::synthetic(id, family, quantity)
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

fn apply_profiled_regime_routing(
    candidates: &mut OptionsCandidateSet,
    context: &RegimeContext,
) -> RegimeRoutingSummary {
    let initial_candidates = candidates.ranked_entries.len();
    candidates
        .ranked_entries
        .retain(|entry| !context.blocks_entry(entry.selected_entry()));
    let routed_candidates = candidates.ranked_entries.len();

    RegimeRoutingSummary {
        initial_candidates,
        routed_candidates,
        blocked_candidates: initial_candidates.saturating_sub(routed_candidates),
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
    let mut payload = json!({
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
    });
    report.profile.insert_json_fields(&mut payload);
    payload
}

fn selected_entry_payload(entry: &ProfiledOptionsEntry) -> Value {
    let descriptor = entry.descriptor();
    let mut payload = json!({
        "strategy": descriptor.strategy,
        "underlying": descriptor.underlying,
        "candidate_type": descriptor.candidate_type,
        "symbols": descriptor.symbols,
        "score": descriptor.score,
        "premium_kind": descriptor.premium_kind.as_str(),
        "premium": descriptor.premium,
    });
    entry.insert_profile_json_fields(&mut payload);
    payload
}

fn option_type_param_for_required_sides(
    required_sides: OptionUniverseRequiredSides,
) -> Option<&'static str> {
    match required_sides {
        OptionUniverseRequiredSides::Calls => Some("call"),
        OptionUniverseRequiredSides::Puts => Some("put"),
        OptionUniverseRequiredSides::CallsAndPuts => None,
    }
}

fn market_trade_naive_date(timezone: Tz) -> NaiveDate {
    Utc::now().with_timezone(&timezone).date_naive()
}

fn market_trade_date(timezone: Tz) -> String {
    market_trade_naive_date(timezone)
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

fn option_universe_contract_from_instrument(
    instrument: &InstrumentAny,
) -> Option<OptionUniverseContract> {
    let underlying = instrument.underlying()?;
    let option_kind = instrument.option_kind()?;
    let expiration_ns = instrument.expiration_ns()?;
    if instrument.strike_price().is_none() {
        return None;
    }

    let series_id = OptionSeriesId::new(
        instrument.venue(),
        underlying,
        instrument.settlement_currency().code,
        expiration_ns,
    );
    Some(OptionUniverseContract::new(
        instrument.id(),
        series_id,
        option_kind,
    ))
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
    strategy_profiles: Option<Vec<AlpacaOptionsStrategyProfile>>,
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
    let mut scan_config = config.clone();
    if let Some(strategy_profiles) = &job.strategy_profiles {
        scan_config.strategy_profiles = strategy_profiles.clone();
    }

    let event_load_events = scan_config
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
        event_load_block_days_before: scan_config.event_shock_block_days_before_earnings,
        event_load_block_days_after: scan_config.event_shock_block_days_after_earnings,
    };
    let feature_snapshot = regime_feature_snapshot_from_option_chain(
        &job.slice,
        &scan_config.regime_features,
        feature_inputs,
        &job.trade_date,
        job.ts_init,
    );
    let regime_context = regime_context_from_features(&feature_snapshot);
    let input = option_chain_candidate_input(&job.slice);
    let candidates = scan_option_chain_candidate_input(
        &input,
        &scan_config,
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
