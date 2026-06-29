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

//! Runtime environment loading for Alpaca options utilities.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const ACCOUNT_COMMAND_PASSTHROUGH_ENV: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "XDG_CONFIG_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_STATE_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "RUST_LOG",
];

/// Loads the Alpaca options environment file when present.
///
/// Repo `.env` is the default local and deployed environment source. Existing process environment
/// values are preserved for manual operator overrides. An explicit `NAUTILUS_ALPACA_ENV_FILE` is
/// treated as an account boundary, so that file's values override inherited values for keys it
/// declares.
///
/// # Errors
///
/// Returns an error if `NAUTILUS_ALPACA_ENV_FILE` points to a missing/unreadable file, the discovered
/// `.env` cannot be inspected, or the env file is invalid.
pub fn load_options_env_file() -> anyhow::Result<Option<PathBuf>> {
    if let Some(path) = env::var_os("NAUTILUS_ALPACA_ENV_FILE").map(PathBuf::from) {
        return load_env_file(&path, true);
    }

    let Some(path) = default_options_env_path()? else {
        return Ok(None);
    };
    load_env_file(&path, false)
}

fn load_env_file(path: &Path, explicit: bool) -> anyhow::Result<Option<PathBuf>> {
    match path.try_exists() {
        Ok(true) => {
            let result = if explicit {
                dotenvy::from_path_override(path)
            } else {
                dotenvy::from_path(path)
            };
            result.map_err(|error| {
                anyhow::anyhow!("failed to load Alpaca env file {}: {error}", path.display())
            })?;
            Ok(Some(path.to_path_buf()))
        }
        Ok(false) if explicit => {
            anyhow::bail!("missing Alpaca env file {}", path.display())
        }
        Ok(false) => Ok(None),
        Err(error) => anyhow::bail!(
            "failed to inspect Alpaca env file {}: {error}",
            path.display()
        ),
    }
}

pub(crate) fn default_options_env_path() -> anyhow::Result<Option<PathBuf>> {
    if let Some(repo) = env::var_os("NAUTILUS_ALPACA_REPO") {
        let path = PathBuf::from(repo).join(".env");
        return match path.try_exists() {
            Ok(true) => Ok(Some(path)),
            Ok(false) => Ok(None),
            Err(error) => anyhow::bail!(
                "failed to inspect Alpaca repo env file {}: {error}",
                path.display()
            ),
        };
    }

    let current_dir = env::current_dir().map_err(|error| {
        anyhow::anyhow!("failed to inspect current directory for Alpaca .env: {error}")
    })?;
    for dir in current_dir.ancestors() {
        let path = dir.join(".env");
        match path.try_exists() {
            Ok(true) => return Ok(Some(path)),
            Ok(false) => {}
            Err(error) => anyhow::bail!(
                "failed to inspect Alpaca repo env file {}: {error}",
                path.display()
            ),
        }
    }
    Ok(None)
}

/// Configures a child process to run with one account's env file as its runtime boundary.
///
/// This intentionally avoids inheriting `ALPACA_*` and `NAUTILUS_ALPACA_*` values from the parent
/// shell or process manager. The child receives only a small set of OS/session variables required
/// for path expansion, TLS/proxy handling, and `systemctl --user`, then the account env file values.
///
/// # Errors
///
/// Returns an error if the account env file cannot be read or parsed.
pub fn configure_account_command_env(command: &mut Command, env_file: &Path) -> anyhow::Result<()> {
    command.env_clear();
    for key in ACCOUNT_COMMAND_PASSTHROUGH_ENV {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    command.env("NAUTILUS_ALPACA_ENV_FILE", env_file);

    let iter = dotenvy::from_path_iter(env_file).map_err(|error| {
        anyhow::anyhow!(
            "failed to read Alpaca env file {}: {error}",
            env_file.display()
        )
    })?;
    for item in iter {
        let (key, value) = item.map_err(|error| {
            anyhow::anyhow!("invalid Alpaca env file {}: {error}", env_file.display())
        })?;
        command.env(key, value);
    }
    Ok(())
}
