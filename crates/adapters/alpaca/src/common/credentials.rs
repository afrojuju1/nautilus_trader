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

//! Alpaca credential helpers.

use std::fmt;

use super::consts::{
    ENV_ALPACA_API_KEY, ENV_ALPACA_API_SECRET, ENV_ALPACA_SECRET_KEY, ENV_APCA_API_KEY_ID,
    ENV_APCA_API_SECRET_KEY,
};

/// Alpaca API key and secret pair.
#[derive(Clone, Eq, PartialEq)]
pub struct AlpacaCredential {
    api_key: Box<str>,
    api_secret: Box<str>,
}

impl fmt::Debug for AlpacaCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AlpacaCredential")
            .field("api_key", &self.masked_api_key())
            .field("api_secret", &"***")
            .finish()
    }
}

impl AlpacaCredential {
    /// Creates a new credential from the provided API key and secret.
    #[must_use]
    pub fn new(api_key: impl Into<String>, api_secret: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into().into_boxed_str(),
            api_secret: api_secret.into().into_boxed_str(),
        }
    }

    /// Resolves credentials from explicit values or supported environment variables.
    #[must_use]
    pub fn resolve(api_key: Option<String>, api_secret: Option<String>) -> Option<Self> {
        let key = api_key.or_else(|| first_env_value(&[ENV_APCA_API_KEY_ID, ENV_ALPACA_API_KEY]));
        let secret = api_secret.or_else(|| {
            first_env_value(&[
                ENV_APCA_API_SECRET_KEY,
                ENV_ALPACA_SECRET_KEY,
                ENV_ALPACA_API_SECRET,
            ])
        });

        match (key, secret) {
            (Some(key), Some(secret)) => Some(Self::new(key, secret)),
            _ => None,
        }
    }

    /// Returns the API key.
    #[must_use]
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Returns the API secret.
    #[must_use]
    pub fn api_secret(&self) -> &str {
        &self.api_secret
    }

    /// Returns the API key with the middle redacted for logs.
    #[must_use]
    pub fn masked_api_key(&self) -> String {
        mask_secret(&self.api_key)
    }
}

fn first_env_value(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn mask_secret(value: &str) -> String {
    let len = value.chars().count();
    if len <= 8 {
        return "*".repeat(len.max(1));
    }

    let prefix: String = value.chars().take(4).collect();
    let suffix: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    format!("{prefix}...{suffix}")
}
