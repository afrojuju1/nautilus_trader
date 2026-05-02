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

//! WebSocket client for Alpaca account trade updates.

use std::{
    fmt::Debug,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};

use nautilus_common::live::get_runtime;
use nautilus_network::{
    mode::ConnectionMode,
    websocket::{TransportBackend, WebSocketClient, WebSocketConfig, channel_message_handler},
};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    common::credentials::AlpacaCredential,
    websocket::{
        messages::{AlpacaAuthRequest, AlpacaListenRequest, AlpacaWsMessage, TRADE_UPDATES_STREAM},
        parse::parse_alpaca_ws_message,
    },
};

const WS_HEARTBEAT_SECS: u64 = 30;
const RECONNECT_TIMEOUT_MS: u64 = 10_000;
const RECONNECT_DELAY_INITIAL_MS: u64 = 1_000;
const RECONNECT_DELAY_MAX_MS: u64 = 30_000;
const RECONNECT_BACKOFF_FACTOR: f64 = 2.0;
const RECONNECT_JITTER_MS: u64 = 250;
const WS_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// WebSocket client for Alpaca account trade updates.
pub struct AlpacaTradeUpdatesWebSocketClient {
    url: String,
    credential: AlpacaCredential,
    connection_mode: Option<Arc<AtomicU8>>,
    signal: Arc<AtomicBool>,
    cmd_tx: Option<mpsc::UnboundedSender<HandlerCommand>>,
    out_rx: Option<mpsc::UnboundedReceiver<AlpacaWsMessage>>,
    task_handle: Option<JoinHandle<()>>,
    transport_backend: TransportBackend,
    proxy_url: Option<String>,
}

impl Debug for AlpacaTradeUpdatesWebSocketClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(AlpacaTradeUpdatesWebSocketClient))
            .field("url", &self.url)
            .field("credential", &self.credential)
            .field("connection_mode", &self.connection_mode())
            .field("transport_backend", &self.transport_backend)
            .field("proxy_url", &self.proxy_url)
            .finish()
    }
}

impl AlpacaTradeUpdatesWebSocketClient {
    /// Creates a new Alpaca trade-updates WebSocket client.
    #[must_use]
    pub fn new(url: impl Into<String>, credential: AlpacaCredential) -> Self {
        Self {
            url: url.into(),
            credential,
            connection_mode: None,
            signal: Arc::new(AtomicBool::new(false)),
            cmd_tx: None,
            out_rx: None,
            task_handle: None,
            transport_backend: TransportBackend::default(),
            proxy_url: None,
        }
    }

    /// Establishes the WebSocket connection and starts parsing account messages.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection cannot be established or the handler cannot be started.
    pub async fn connect(&mut self) -> anyhow::Result<()> {
        if self.is_active() || self.is_reconnecting() {
            log::warn!("Alpaca trade updates WebSocket already connected or reconnecting");
            return Ok(());
        }

        self.signal.store(false, Ordering::Relaxed);

        let (message_handler, raw_rx) = channel_message_handler();
        let cfg = WebSocketConfig {
            url: self.url.clone(),
            headers: vec![],
            heartbeat: Some(WS_HEARTBEAT_SECS),
            heartbeat_msg: None,
            reconnect_timeout_ms: Some(RECONNECT_TIMEOUT_MS),
            reconnect_delay_initial_ms: Some(RECONNECT_DELAY_INITIAL_MS),
            reconnect_delay_max_ms: Some(RECONNECT_DELAY_MAX_MS),
            reconnect_backoff_factor: Some(RECONNECT_BACKOFF_FACTOR),
            reconnect_jitter_ms: Some(RECONNECT_JITTER_MS),
            reconnect_max_attempts: None,
            idle_timeout_ms: None,
            backend: self.transport_backend,
            proxy_url: self.proxy_url.clone(),
        };

        let client =
            WebSocketClient::connect(cfg, Some(message_handler), None, None, Vec::new(), None)
                .await?;
        self.connection_mode = Some(client.connection_mode_atomic());

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<HandlerCommand>();
        let (out_tx, out_rx) = mpsc::unbounded_channel::<AlpacaWsMessage>();
        self.cmd_tx = Some(cmd_tx.clone());
        self.out_rx = Some(out_rx);

        let signal = Arc::clone(&self.signal);
        let credential = self.credential.clone();

        let handle = get_runtime().spawn(async move {
            let mut handler = TradeUpdatesHandler::new(signal, credential, cmd_rx, raw_rx);

            if let Err(e) = cmd_tx.send(HandlerCommand::SetClient(client)) {
                log::debug!("Alpaca trade updates handler command channel closed: {e}");
                return;
            }

            loop {
                match handler.next().await {
                    Some(message) => {
                        if let Err(e) = out_tx.send(message) {
                            log::debug!("Alpaca trade updates output channel closed: {e}");
                            break;
                        }
                    }
                    None => {
                        log::info!("Alpaca trade updates WebSocket handler stopped");
                        break;
                    }
                }
            }
        });

        self.task_handle = Some(handle);
        Ok(())
    }

    /// Disconnects the WebSocket and stops the handler task.
    pub async fn disconnect(&mut self) {
        if let Some(cmd_tx) = &self.cmd_tx
            && let Err(e) = cmd_tx.send(HandlerCommand::Disconnect)
        {
            log::debug!("Failed to send Alpaca WebSocket disconnect command: {e}");
        }

        self.signal.store(true, Ordering::Release);

        if let Some(handle) = self.task_handle.take() {
            let abort_handle = handle.abort_handle();
            match tokio::time::timeout(WS_DISCONNECT_TIMEOUT, handle).await {
                Ok(_) => log::debug!("Alpaca trade updates handler task completed"),
                Err(_) => {
                    log::warn!("Alpaca trade updates handler task did not stop, aborting");
                    abort_handle.abort();
                }
            }
        }

        self.cmd_tx = None;
        self.connection_mode = None;
    }

    /// Returns true if the underlying network connection is active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.connection_mode
            .as_ref()
            .is_some_and(|mode| ConnectionMode::from_u8(mode.load(Ordering::Relaxed)).is_active())
    }

    /// Returns true if the underlying network connection is reconnecting.
    #[must_use]
    pub fn is_reconnecting(&self) -> bool {
        self.connection_mode.as_ref().is_some_and(|mode| {
            ConnectionMode::from_u8(mode.load(Ordering::Relaxed)).is_reconnect()
        })
    }

    fn connection_mode(&self) -> Option<ConnectionMode> {
        self.connection_mode
            .as_ref()
            .map(|mode| ConnectionMode::from_u8(mode.load(Ordering::Relaxed)))
    }

    /// Takes the output message receiver, leaving `None` in its place.
    #[must_use]
    pub fn take_out_rx(&mut self) -> Option<mpsc::UnboundedReceiver<AlpacaWsMessage>> {
        self.out_rx.take()
    }
}

enum HandlerCommand {
    SetClient(WebSocketClient),
    Disconnect,
}

impl Debug for HandlerCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SetClient(_) => f.write_str("SetClient"),
            Self::Disconnect => f.write_str("Disconnect"),
        }
    }
}

#[derive(Debug)]
struct TradeUpdatesHandler {
    signal: Arc<AtomicBool>,
    credential: AlpacaCredential,
    client: Option<WebSocketClient>,
    cmd_rx: mpsc::UnboundedReceiver<HandlerCommand>,
    raw_rx: mpsc::UnboundedReceiver<Message>,
}

impl TradeUpdatesHandler {
    fn new(
        signal: Arc<AtomicBool>,
        credential: AlpacaCredential,
        cmd_rx: mpsc::UnboundedReceiver<HandlerCommand>,
        raw_rx: mpsc::UnboundedReceiver<Message>,
    ) -> Self {
        Self {
            signal,
            credential,
            client: None,
            cmd_rx,
            raw_rx,
        }
    }

    async fn next(&mut self) -> Option<AlpacaWsMessage> {
        loop {
            if self.signal.load(Ordering::Acquire) {
                return None;
            }

            tokio::select! {
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        HandlerCommand::SetClient(client) => {
                            self.client = Some(client);
                            self.authenticate_and_listen().await;
                        }
                        HandlerCommand::Disconnect => {
                            if let Some(client) = self.client.take() {
                                client.notify_closed();
                            }
                            return None;
                        }
                    }
                }
                Some(raw) = self.raw_rx.recv() => {
                    match raw {
                        Message::Ping(data) => {
                            if let Some(client) = &self.client
                                && let Err(e) = client.send_pong(data.to_vec()).await
                            {
                                log::warn!("Failed to send Alpaca WebSocket pong: {e}");
                            }
                        }
                        Message::Close(_) => return None,
                        other => match parse_alpaca_ws_message(&other) {
                            Ok(Some(AlpacaWsMessage::Reconnected)) => {
                                self.authenticate_and_listen().await;
                                return Some(AlpacaWsMessage::Reconnected);
                            }
                            Ok(Some(message)) => return Some(message),
                            Ok(None) => {}
                            Err(e) => {
                                return Some(AlpacaWsMessage::Error(format!(
                                    "failed to parse Alpaca WebSocket message: {e}",
                                )));
                            }
                        },
                    }
                }
                else => return None,
            }
        }
    }

    async fn authenticate_and_listen(&self) {
        let Some(client) = &self.client else {
            log::warn!("Cannot authenticate Alpaca WebSocket before client is set");
            return;
        };

        let auth = AlpacaAuthRequest::new(self.credential.api_key(), self.credential.api_secret());
        match serde_json::to_string(&auth) {
            Ok(json) => {
                if let Err(e) = client.send_text(json, None).await {
                    log::warn!("Failed to send Alpaca WebSocket auth request: {e}");
                    return;
                }
            }
            Err(e) => {
                log::warn!("Failed to serialize Alpaca WebSocket auth request: {e}");
                return;
            }
        }

        let streams = [TRADE_UPDATES_STREAM];
        let listen = AlpacaListenRequest::new(&streams);
        match serde_json::to_string(&listen) {
            Ok(json) => {
                if let Err(e) = client.send_text(json, None).await {
                    log::warn!("Failed to send Alpaca WebSocket listen request: {e}");
                }
            }
            Err(e) => log::warn!("Failed to serialize Alpaca WebSocket listen request: {e}"),
        }
    }
}
