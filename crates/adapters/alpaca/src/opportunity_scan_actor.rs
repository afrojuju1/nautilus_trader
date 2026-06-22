//! Read-only Nautilus actor for option-chain candidate evidence.

use chrono::{Datelike, NaiveDate, Utc};
use nautilus_common::{
    actor::{DataActor, DataActorConfig, DataActorCore},
    nautilus_actor,
};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::option_chain::{OptionChainSlice, StrikeRange},
    identifiers::{ActorId, ClientId, OptionSeriesId},
};
use serde_json::{Value, json};

use crate::{
    candidate_engine::{
        CreditSpreadKind, DebitSpreadKind, DebitSpreadScannerConfig, IronCondorScannerConfig,
        NakedOptionCapitalContext, NakedOptionKind, NakedOptionScannerConfig,
        PutCreditScannerConfig,
    },
    option_chain_candidates::{
        option_chain_candidate_input, scan_credit_spread_option_chain,
        scan_debit_spread_option_chain, scan_iron_condor_option_chain, scan_naked_option_chain,
    },
    options_entry::{
        SelectedDebitEntry, SelectedEntry, SelectedIronCondorEntry, SelectedNakedOptionEntry,
        SelectedOptionsEntry,
    },
    options_runtime::{OptionsOpportunitySet, OptionsScanOutcome, OptionsScanReport},
    runtime::{
        credit_spread_strategy_name, debit_spread_strategy_name, emit_operator_event,
        naked_option_strategy_name,
    },
};

/// Read-only scan settings for candidate discovery from option-chain slices.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionChainOpportunityScanConfig {
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
}

impl Default for OptionChainOpportunityScanConfig {
    fn default() -> Self {
        Self {
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
        }
    }
}

/// Actor configuration for read-only option-chain opportunity scans.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionChainOpportunityScanActorConfig {
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
    /// Candidate scan settings.
    pub scan: OptionChainOpportunityScanConfig,
}

impl Default for OptionChainOpportunityScanActorConfig {
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
            scan: OptionChainOpportunityScanConfig::default(),
        }
    }
}

/// Read-only actor that ranks option-chain candidates and emits operator evidence.
#[derive(Debug)]
pub struct OptionChainOpportunityScanActor {
    core: DataActorCore,
    config: OptionChainOpportunityScanActorConfig,
    latest_opportunities: Option<OptionsOpportunitySet>,
}

nautilus_actor!(OptionChainOpportunityScanActor);

impl OptionChainOpportunityScanActor {
    /// Creates a new read-only option-chain opportunity scan actor.
    #[must_use]
    pub fn new(config: OptionChainOpportunityScanActorConfig) -> Self {
        let core = DataActorCore::new(DataActorConfig {
            actor_id: config.actor_id.clone(),
            ..Default::default()
        });
        Self {
            core,
            config,
            latest_opportunities: None,
        }
    }

    /// Returns the most recent opportunity set produced by this actor.
    #[must_use]
    pub fn latest_opportunities(&self) -> Option<&OptionsOpportunitySet> {
        self.latest_opportunities.as_ref()
    }
}

impl DataActor for OptionChainOpportunityScanActor {
    fn on_start(&mut self) -> anyhow::Result<()> {
        if self.config.series.is_empty() {
            log::warn!("Option-chain opportunity scan actor has no series subscriptions");
            return Ok(());
        }

        for series_id in self.config.series.clone() {
            self.subscribe_option_chain(
                series_id,
                self.config.strike_range.clone(),
                self.config.snapshot_interval_ms,
                self.config.client_id.clone(),
                None,
            );
        }
        Ok(())
    }

    fn on_option_chain(&mut self, slice: &OptionChainSlice) -> anyhow::Result<()> {
        let scan_date = scan_date_from_timestamp(slice.ts_event);
        let trade_date = scan_date.format("%Y-%m-%d").to_string();
        let opportunities = scan_option_chain_opportunities(slice, &self.config.scan, &trade_date);
        emit_operator_event(
            "option_chain_opportunity_scan",
            opportunity_event_payload(slice, &opportunities),
        );
        self.latest_opportunities = Some(opportunities);
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        for series_id in self.config.series.clone() {
            self.unsubscribe_option_chain(series_id, self.config.client_id.clone());
        }
        Ok(())
    }
}

/// Discovers ranked option opportunities from one Nautilus option-chain slice.
#[must_use]
pub fn scan_option_chain_opportunities(
    slice: &OptionChainSlice,
    config: &OptionChainOpportunityScanConfig,
    trade_date: &str,
) -> OptionsOpportunitySet {
    let input = option_chain_candidate_input(slice);
    let scan_date = scan_date_from_timestamp(slice.ts_event);
    let mut opportunities = OptionsOpportunitySet::new(trade_date);

    for kind in &config.spread_kinds {
        let result =
            scan_credit_spread_option_chain(&input, &config.credit_scanner, *kind, scan_date);
        let strategy_name = credit_spread_strategy_name(*kind);
        opportunities.push_scan(OptionsScanReport::new(
            &input.underlying,
            strategy_name,
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            opportunities.consider_candidate(SelectedOptionsEntry::Credit(SelectedEntry {
                underlying: input.underlying.clone(),
                kind: *kind,
                candidate: best.clone(),
            }));
        }
    }

    if config.iron_condor_enabled {
        let result = scan_iron_condor_option_chain(&input, &config.iron_condor_scanner, scan_date);
        opportunities.push_scan(OptionsScanReport::new(
            &input.underlying,
            "iron_condor",
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            opportunities.consider_candidate(SelectedOptionsEntry::IronCondor(
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
        opportunities.push_scan(OptionsScanReport::new(
            &input.underlying,
            strategy_name,
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            opportunities.consider_candidate(SelectedOptionsEntry::Debit(SelectedDebitEntry {
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
        opportunities.push_scan(OptionsScanReport::new(
            &input.underlying,
            strategy_name,
            result.candidates.len(),
            result.contract_count,
            result.snapshot_count,
            result.scoreable_count,
            result.rejection_counts.clone(),
        ));
        if let Some(best) = result.candidates.first() {
            opportunities.consider_candidate(SelectedOptionsEntry::NakedOption(
                SelectedNakedOptionEntry {
                    underlying: input.underlying.clone(),
                    kind: *kind,
                    candidate: best.clone(),
                },
            ));
        }
    }

    opportunities
}

fn naked_scanner_for(
    config: &OptionChainOpportunityScanConfig,
    kind: NakedOptionKind,
) -> &NakedOptionScannerConfig {
    if kind.is_one_to_three_dte() {
        &config.naked_1_3dte_scanner
    } else {
        &config.naked_scanner
    }
}

fn opportunity_event_payload(
    slice: &OptionChainSlice,
    opportunities: &OptionsOpportunitySet,
) -> Value {
    let input = option_chain_candidate_input(slice);
    json!({
        "source": "option_chain",
        "series_id": slice.series_id.to_string(),
        "underlying": input.underlying,
        "expiration_date": input.expiration_date,
        "underlying_price": input.underlying_price,
        "call_contracts": input.calls.contract_count(),
        "put_contracts": input.puts.contract_count(),
        "scans": opportunities.scans.iter().map(scan_report_payload).collect::<Vec<_>>(),
        "ranked_entries": opportunities.ranked_entries().len(),
        "selected": opportunities.selected_entry().map(selected_entry_payload),
    })
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

fn scan_date_from_timestamp(ts_event: UnixNanos) -> NaiveDate {
    let date = ts_event.to_datetime_utc().date_naive();
    if date.year() < 2000 {
        Utc::now().date_naive()
    } else {
        date
    }
}
