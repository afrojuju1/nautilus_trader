// -------------------------------------------------------------------------------------------------
//  Copyright (C) 2015-2026 Nautech Systems Pty Ltd. All rights reserved.
//  https://nautechsystems.io
//
//  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
//  You may not use this file except in compliance with the License.
//  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
// -------------------------------------------------------------------------------------------------

//! Authenticated Alpaca REST client.

use std::time::Duration;

use reqwest::{
    StatusCode, Url,
    header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT},
};
use serde::{Serialize, de::DeserializeOwned};
use url::form_urlencoded;

use crate::{
    common::{credentials::AlpacaCredential, urls::DATA_BASE_URL},
    config::{AlpacaDataClientConfig, AlpacaExecClientConfig},
    http::{
        error::{Error, Result},
        models::{
            AlpacaAccount, AlpacaActivity, AlpacaOrder, AlpacaPosition, ListActivitiesRequest,
            ListOptionContractsRequest, ListOrdersRequest, OptionBarsRequest, OptionBarsResponse,
            OptionContractsResponse, OptionSnapshotsRequest, OptionSnapshotsResponse,
            OptionTradesRequest, OptionTradesResponse, ReplaceOrderRequest,
            StockBarsRequest, StockBarsResponse, StockSnapshotsRequest, StockSnapshotsResponse,
        },
    },
    orders::{MlegOrderPayload, SimpleOrderPayload},
};

const APCA_API_KEY_HEADER: &str = "APCA-API-KEY-ID";
const APCA_API_SECRET_HEADER: &str = "APCA-API-SECRET-KEY";
const NAUTILUS_ALPACA_USER_AGENT: &str = "nautilus-trader-alpaca/0.57.0";

/// Authenticated HTTP client for Alpaca Trading and Market Data REST APIs.
#[derive(Clone, Debug)]
pub struct AlpacaHttpClient {
    client: reqwest::Client,
    credential: AlpacaCredential,
    trading_base_url: String,
    data_base_url: String,
}

impl AlpacaHttpClient {
    /// Creates a client from an Alpaca data client config.
    ///
    /// # Errors
    ///
    /// Returns an error when credentials cannot be resolved or the HTTP client cannot be built.
    pub fn from_data_config(config: &AlpacaDataClientConfig) -> Result<Self> {
        let credential =
            AlpacaCredential::resolve(config.api_key.clone(), config.api_secret.clone())
                .ok_or(Error::MissingCredentials)?;

        Self::new(
            credential,
            config.resolved_trading_base_url(),
            config.resolved_data_base_url(),
            config.request_timeout_secs,
        )
    }

    /// Creates a client from an Alpaca execution client config.
    ///
    /// # Errors
    ///
    /// Returns an error when credentials cannot be resolved or the HTTP client cannot be built.
    pub fn from_exec_config(config: &AlpacaExecClientConfig) -> Result<Self> {
        let credential =
            AlpacaCredential::resolve(config.api_key.clone(), config.api_secret.clone())
                .ok_or(Error::MissingCredentials)?;

        Self::new(
            credential,
            config.resolved_trading_base_url(),
            DATA_BASE_URL,
            config.request_timeout_secs,
        )
    }

    /// Creates a client from explicit credentials and base URLs.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot be built.
    pub fn new(
        credential: AlpacaCredential,
        trading_base_url: impl Into<String>,
        data_base_url: impl Into<String>,
        timeout_secs: u64,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .default_headers(default_headers(&credential)?)
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| Error::ClientBuild(e.to_string()))?;

        Ok(Self {
            client,
            credential,
            trading_base_url: trading_base_url.into().trim_end_matches('/').to_string(),
            data_base_url: data_base_url.into().trim_end_matches('/').to_string(),
        })
    }

    /// Returns a masked API key for diagnostics.
    #[must_use]
    pub fn api_key_masked(&self) -> String {
        self.credential.masked_api_key()
    }

    /// Returns the configured trading REST base URL.
    #[must_use]
    pub fn trading_base_url(&self) -> &str {
        &self.trading_base_url
    }

    /// Returns the configured market data REST base URL.
    #[must_use]
    pub fn data_base_url(&self) -> &str {
        &self.data_base_url
    }

    /// Lists one page of option contracts from Alpaca's Trading API.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn list_option_contracts_page(
        &self,
        request: &ListOptionContractsRequest,
    ) -> Result<OptionContractsResponse> {
        self.get_trading_json("/v2/options/contracts", &request.query_pairs())
            .await
    }

    /// Lists all option contracts matching the request, following page tokens.
    ///
    /// # Errors
    ///
    /// Returns an error if any page request fails or cannot be decoded.
    pub async fn list_option_contracts(
        &self,
        request: &ListOptionContractsRequest,
    ) -> Result<OptionContractsResponse> {
        let mut contracts = Vec::new();
        let mut page_token = request.page_token.clone();

        loop {
            let page_request = request.with_page_token(page_token.clone());
            let mut page = self.list_option_contracts_page(&page_request).await?;
            page_token = page.next_token();
            contracts.append(&mut page.option_contracts);

            if page_token.is_none() {
                break;
            }
        }

        Ok(OptionContractsResponse {
            option_contracts: contracts,
            next_page_token: None,
            page_token: None,
        })
    }

    /// Lists one page of option snapshots from Alpaca's Market Data API.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn option_snapshots_page(
        &self,
        request: &OptionSnapshotsRequest,
    ) -> Result<OptionSnapshotsResponse> {
        self.get_data_json("/v1beta1/options/snapshots", &request.query_pairs())
            .await
    }

    /// Lists option snapshots for all symbols in the request, batching at Alpaca's 100-symbol cap.
    ///
    /// # Errors
    ///
    /// Returns an error if any request fails or cannot be decoded.
    pub async fn option_snapshots(
        &self,
        request: &OptionSnapshotsRequest,
    ) -> Result<OptionSnapshotsResponse> {
        let mut snapshots = std::collections::BTreeMap::new();

        for symbol_batch in request.symbols.chunks(100) {
            let mut page_token = request.page_token.clone();

            loop {
                let page_request =
                    request.with_symbols_and_page(symbol_batch.iter().cloned(), page_token.clone());
                let mut page = self.option_snapshots_page(&page_request).await?;
                page_token = page.next_token();
                snapshots.append(&mut page.snapshots);

                if page_token.is_none() {
                    break;
                }
            }
        }

        Ok(OptionSnapshotsResponse {
            snapshots,
            next_page_token: None,
            page_token: None,
        })
    }

    /// Lists one page of historical option bars from Alpaca's Market Data API.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn option_bars_page(
        &self,
        request: &OptionBarsRequest,
    ) -> Result<OptionBarsResponse> {
        self.get_data_json("/v1beta1/options/bars", &request.query_pairs())
            .await
    }

    /// Lists historical option bars for all symbols in the request, batching at Alpaca's 100-symbol cap.
    ///
    /// # Errors
    ///
    /// Returns an error if any request fails or cannot be decoded.
    pub async fn option_bars(&self, request: &OptionBarsRequest) -> Result<OptionBarsResponse> {
        let mut bars = std::collections::BTreeMap::new();

        for symbol_batch in request.symbols.chunks(100) {
            let mut page_token = request.page_token.clone();

            loop {
                let page_request =
                    request.with_symbols_and_page(symbol_batch.iter().cloned(), page_token.clone());
                let page = self.option_bars_page(&page_request).await?;
                page_token = page.next_token();

                for (symbol, mut symbol_bars) in page.bars {
                    bars.entry(symbol).or_default().append(&mut symbol_bars);
                }

                if page_token.is_none() {
                    break;
                }
            }
        }

        Ok(OptionBarsResponse {
            bars,
            next_page_token: None,
        })
    }

    /// Lists one page of historical option trades from Alpaca's Market Data API.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn option_trades_page(
        &self,
        request: &OptionTradesRequest,
    ) -> Result<OptionTradesResponse> {
        self.get_data_json("/v1beta1/options/trades", &request.query_pairs())
            .await
    }

    /// Lists historical option trades for all symbols in the request, batching at Alpaca's 100-symbol cap.
    ///
    /// # Errors
    ///
    /// Returns an error if any request fails or cannot be decoded.
    pub async fn option_trades(
        &self,
        request: &OptionTradesRequest,
    ) -> Result<OptionTradesResponse> {
        let mut trades = std::collections::BTreeMap::new();

        for symbol_batch in request.symbols.chunks(100) {
            let mut page_token = request.page_token.clone();

            loop {
                let page_request =
                    request.with_symbols_and_page(symbol_batch.iter().cloned(), page_token.clone());
                let page = self.option_trades_page(&page_request).await?;
                page_token = page.next_token();

                for (symbol, mut symbol_trades) in page.trades {
                    trades.entry(symbol).or_default().append(&mut symbol_trades);
                }

                if page_token.is_none() {
                    break;
                }
            }
        }

        Ok(OptionTradesResponse {
            trades,
            next_page_token: None,
        })
    }

    /// Lists one page of historical stock bars from Alpaca's Market Data API.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn stock_bars_page(&self, request: &StockBarsRequest) -> Result<StockBarsResponse> {
        self.get_data_json("/v2/stocks/bars", &request.query_pairs())
            .await
    }

    /// Lists historical stock bars for all symbols in the request, batching at Alpaca's 100-symbol cap.
    ///
    /// # Errors
    ///
    /// Returns an error if any request fails or cannot be decoded.
    pub async fn stock_bars(&self, request: &StockBarsRequest) -> Result<StockBarsResponse> {
        let mut bars = std::collections::BTreeMap::new();

        for symbol_batch in request.symbols.chunks(100) {
            let mut page_token = request.page_token.clone();

            loop {
                let page_request =
                    request.with_symbols_and_page(symbol_batch.iter().cloned(), page_token.clone());
                let page = self.stock_bars_page(&page_request).await?;
                page_token = page.next_token();

                for (symbol, mut symbol_bars) in page.bars {
                    bars.entry(symbol).or_default().append(&mut symbol_bars);
                }

                if page_token.is_none() {
                    break;
                }
            }
        }

        Ok(StockBarsResponse {
            bars,
            next_page_token: None,
        })
    }

    /// Lists stock snapshots from Alpaca's Market Data API.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn stock_snapshots(
        &self,
        request: &StockSnapshotsRequest,
    ) -> Result<StockSnapshotsResponse> {
        self.get_data_json("/v2/stocks/snapshots", &request.query_pairs())
            .await
    }

    /// Returns the current Alpaca trading account.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn account(&self) -> Result<AlpacaAccount> {
        self.get_trading_json("/v2/account", &[]).await
    }

    /// Lists open positions for the current Alpaca trading account.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn positions(&self) -> Result<Vec<AlpacaPosition>> {
        self.get_trading_json("/v2/positions", &[]).await
    }

    /// Lists orders for the current Alpaca trading account.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn orders(&self, request: &ListOrdersRequest) -> Result<Vec<AlpacaOrder>> {
        self.get_trading_json("/v2/orders", &request.query_pairs())
            .await
    }

    /// Retrieves one order by broker order ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the order ID is empty, the request fails, or the response cannot be
    /// decoded.
    pub async fn order_by_id(&self, order_id: &str, nested: bool) -> Result<AlpacaOrder> {
        if order_id.trim().is_empty() {
            return Err(Error::Validation("order_id must not be empty".to_string()));
        }
        let path = format!("/v2/orders/{}", order_id.trim());
        let query_pairs = if nested {
            vec![("nested", "true".to_string())]
        } else {
            Vec::new()
        };
        self.get_trading_json(&path, &query_pairs).await
    }

    /// Retrieves one order by client order ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the client order ID is empty, the request fails, or the response cannot
    /// be decoded.
    pub async fn order_by_client_order_id(
        &self,
        client_order_id: &str,
        nested: bool,
    ) -> Result<AlpacaOrder> {
        if client_order_id.trim().is_empty() {
            return Err(Error::Validation(
                "client_order_id must not be empty".to_string(),
            ));
        }
        let mut query_pairs = vec![("client_order_id", client_order_id.trim().to_string())];
        if nested {
            query_pairs.push(("nested", "true".to_string()));
        }
        self.get_trading_json("/v2/orders:by_client_order_id", &query_pairs)
            .await
    }

    /// Lists account activities for reconciliation.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the response cannot be decoded.
    pub async fn account_activities(
        &self,
        request: &ListActivitiesRequest,
    ) -> Result<Vec<AlpacaActivity>> {
        self.get_trading_json("/v2/account/activities", &request.query_pairs())
            .await
    }

    /// Lists all account activities for a request by following `page_token` pagination.
    ///
    /// # Errors
    ///
    /// Returns an error if any page request fails or a response cannot be decoded.
    pub async fn account_activities_all(
        &self,
        request: &ListActivitiesRequest,
    ) -> Result<Vec<AlpacaActivity>> {
        let mut page_request = request.clone();
        let mut activities = Vec::new();

        loop {
            let page = self.account_activities(&page_request).await?;
            if page.is_empty() {
                break;
            }

            let next_token = page.last().and_then(|activity| {
                activity
                    .id
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .map(ToString::to_string)
            });
            let page_len = page.len();
            activities.extend(page);

            if page_len < page_request.page_size {
                break;
            }

            let Some(next_token) = next_token else {
                break;
            };
            if page_request.page_token.as_deref() == Some(next_token.as_str()) {
                break;
            }
            page_request = page_request.with_page_token(Some(next_token));
        }

        Ok(activities)
    }

    /// Submits a validated Alpaca multi-leg order payload.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload fails local validation, the request fails, or the response
    /// cannot be decoded.
    pub async fn submit_mleg_order(&self, payload: &MlegOrderPayload) -> Result<AlpacaOrder> {
        payload.validate()?;
        self.post_trading_json("/v2/orders", &[], payload).await
    }

    /// Submits a validated Alpaca simple order payload.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload fails local validation, the request fails, or the response
    /// cannot be decoded.
    pub async fn submit_simple_order(&self, payload: &SimpleOrderPayload) -> Result<AlpacaOrder> {
        payload.validate()?;
        self.post_trading_json("/v2/orders", &[], payload).await
    }

    /// Attempts to cancel an open Alpaca order by broker order ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the order ID is empty, the cancel request fails, or Alpaca rejects the
    /// cancel request.
    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        if order_id.trim().is_empty() {
            return Err(Error::Validation("order_id must not be empty".to_string()));
        }

        let path = format!("/v2/orders/{order_id}");
        self.delete_trading(&path).await
    }

    /// Replaces an existing Alpaca order by broker order ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the order ID is empty, the request has no replacement fields, the
    /// request fails, or Alpaca rejects the replacement.
    pub async fn replace_order(
        &self,
        order_id: &str,
        request: &ReplaceOrderRequest,
    ) -> Result<AlpacaOrder> {
        if order_id.trim().is_empty() {
            return Err(Error::Validation("order_id must not be empty".to_string()));
        }
        if request.qty.is_none()
            && request.time_in_force.is_none()
            && request.limit_price.is_none()
            && request.stop_price.is_none()
            && request.trail.is_none()
            && request.client_order_id.is_none()
        {
            return Err(Error::Validation(
                "replace_order requires at least one replacement field".to_string(),
            ));
        }

        let path = format!("/v2/orders/{order_id}");
        self.patch_trading_json(&path, &[], request).await
    }

    async fn get_trading_json<T>(
        &self,
        path: &str,
        query_pairs: &[(&'static str, String)],
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        self.get_json(&self.trading_base_url, path, query_pairs)
            .await
    }

    async fn post_trading_json<T, B>(
        &self,
        path: &str,
        query_pairs: &[(&'static str, String)],
        body: &B,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.post_json(&self.trading_base_url, path, query_pairs, body)
            .await
    }

    async fn patch_trading_json<T, B>(
        &self,
        path: &str,
        query_pairs: &[(&'static str, String)],
        body: &B,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.patch_json(&self.trading_base_url, path, query_pairs, body)
            .await
    }

    async fn delete_trading(&self, path: &str) -> Result<()> {
        let url = build_url(&self.trading_base_url, path, &[])?;
        let response = self.client.delete(url.clone()).send().await?;
        decode_empty_response(response.status(), url, response.text().await?).await
    }

    async fn get_data_json<T>(
        &self,
        path: &str,
        query_pairs: &[(&'static str, String)],
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let base_url = if self.data_base_url.is_empty() {
            DATA_BASE_URL
        } else {
            &self.data_base_url
        };
        self.get_json(base_url, path, query_pairs).await
    }

    async fn get_json<T>(
        &self,
        base_url: &str,
        path: &str,
        query_pairs: &[(&'static str, String)],
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let url = build_url(base_url, path, query_pairs)?;
        let response = self.client.get(url.clone()).send().await?;
        decode_response(response.status(), url, response.text().await?).await
    }

    async fn post_json<T, B>(
        &self,
        base_url: &str,
        path: &str,
        query_pairs: &[(&'static str, String)],
        body: &B,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let url = build_url(base_url, path, query_pairs)?;
        let response = self.client.post(url.clone()).json(body).send().await?;
        decode_response(response.status(), url, response.text().await?).await
    }

    async fn patch_json<T, B>(
        &self,
        base_url: &str,
        path: &str,
        query_pairs: &[(&'static str, String)],
        body: &B,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let url = build_url(base_url, path, query_pairs)?;
        let response = self.client.patch(url.clone()).json(body).send().await?;
        decode_response(response.status(), url, response.text().await?).await
    }
}

fn default_headers(credential: &AlpacaCredential) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(NAUTILUS_ALPACA_USER_AGENT),
    );
    headers.insert(
        APCA_API_KEY_HEADER,
        HeaderValue::from_str(credential.api_key())
            .map_err(|e| Error::ClientBuild(format!("invalid Alpaca API key header: {e}")))?,
    );
    headers.insert(
        APCA_API_SECRET_HEADER,
        HeaderValue::from_str(credential.api_secret())
            .map_err(|e| Error::ClientBuild(format!("invalid Alpaca API secret header: {e}")))?,
    );
    Ok(headers)
}

fn build_url(base_url: &str, path: &str, query_pairs: &[(&'static str, String)]) -> Result<Url> {
    let base = format!("{}{}", base_url.trim_end_matches('/'), path);
    let mut url = Url::parse(&base)?;
    if !query_pairs.is_empty() {
        let query = {
            let mut serializer = form_urlencoded::Serializer::new(String::new());
            for (key, value) in query_pairs {
                if !value.trim().is_empty() {
                    serializer.append_pair(key, value);
                }
            }
            serializer.finish()
        };
        url.set_query(Some(&query));
    }
    Ok(url)
}

async fn decode_response<T>(status: StatusCode, url: Url, body: String) -> Result<T>
where
    T: DeserializeOwned,
{
    if !status.is_success() {
        return Err(Error::HttpStatus {
            status: status.as_u16(),
            url: url.to_string(),
            body,
        });
    }

    Ok(serde_json::from_str(&body)?)
}

async fn decode_empty_response(status: StatusCode, url: Url, body: String) -> Result<()> {
    if !status.is_success() {
        return Err(Error::HttpStatus {
            status: status.as_u16(),
            url: url.to_string(),
            body,
        });
    }

    Ok(())
}
