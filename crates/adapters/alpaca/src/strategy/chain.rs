//! Option-chain and market snapshot loading shared by strategy scanners.

use std::collections::BTreeMap;

use time::{Duration, OffsetDateTime};

use crate::{
    config::AlpacaDataClientConfig,
    http::{
        client::AlpacaHttpClient,
        error::Result,
        models::{
            AlpacaOptionContract, AlpacaOptionSnapshot, AlpacaOptionType, OptionSnapshotsRequest,
            StockSnapshotsRequest,
        },
    },
    providers::AlpacaOptionContractProvider,
};

#[derive(Clone, Debug)]
pub(super) struct OptionChainSnapshot {
    pub(super) contracts: Vec<AlpacaOptionContract>,
    pub(super) snapshots: BTreeMap<String, AlpacaOptionSnapshot>,
}

pub(super) async fn load_option_chain_snapshot(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    underlying: &str,
    min_dte: i64,
    max_dte: i64,
    option_type: AlpacaOptionType,
) -> Result<OptionChainSnapshot> {
    let today = OffsetDateTime::now_utc().date();
    let min_expiration = (today + Duration::days(min_dte)).to_string();
    let max_expiration = (today + Duration::days(max_dte)).to_string();
    let provider = AlpacaOptionContractProvider::new(client.clone());

    let contracts = provider
        .load_active_contracts(
            underlying.to_string(),
            min_expiration,
            max_expiration,
            Some(option_type),
        )
        .await?;
    let symbols = contracts
        .iter()
        .map(|contract| contract.symbol.clone())
        .collect::<Vec<_>>();
    let mut snapshots_request = OptionSnapshotsRequest::for_symbols(symbols);
    snapshots_request.feed = Some(data_config.option_feed.as_str().to_string());
    let snapshots = client.option_snapshots(&snapshots_request).await?.snapshots;

    Ok(OptionChainSnapshot {
        contracts,
        snapshots,
    })
}

pub(super) async fn load_underlying_price(
    client: &AlpacaHttpClient,
    data_config: &AlpacaDataClientConfig,
    underlying: &str,
) -> Result<f64> {
    let mut request = StockSnapshotsRequest::for_symbols([underlying.to_string()]);
    request.feed = Some(data_config.stock_feed.as_str().to_string());
    let snapshots = client.stock_snapshots(&request).await?.snapshots;
    snapshots
        .get(underlying)
        .or_else(|| {
            snapshots
                .iter()
                .find(|(symbol, _)| symbol.eq_ignore_ascii_case(underlying))
                .map(|(_, snapshot)| snapshot)
        })
        .and_then(|snapshot| snapshot.latest_price())
        .ok_or_else(|| {
            crate::http::error::Error::Validation(format!(
                "stock snapshot missing latest price for {underlying}"
            ))
        })
}
