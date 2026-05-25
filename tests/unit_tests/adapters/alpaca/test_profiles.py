# -------------------------------------------------------------------------------------------------
#  Copyright (C) 2015-2026 Nautech Systems Pty Ltd. All rights reserved.
#  https://nautechsystems.io
#
#  Licensed under the GNU Lesser General Public License Version 3.0 (the "License");
#  You may not use this file except in compliance with the License.
#  You may obtain a copy of the License at https://www.gnu.org/licenses/lgpl-3.0.en.html
#
#  Unless required by applicable law or agreed to in writing, software
#  distributed under the License is distributed on an "AS IS" BASIS,
#  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
#  See the License for the specific language governing permissions and
#  limitations under the License.
# -------------------------------------------------------------------------------------------------

import argparse

from nautilus_trader.adapters.alpaca.profiles import alpaca_env_file_for_profile
from nautilus_trader.adapters.alpaca.profiles import load_alpaca_env_file
from nautilus_trader.adapters.alpaca.profiles import load_alpaca_profile_from_args


def test_alpaca_env_file_for_profile_prefers_account_env(tmp_path) -> None:
    account_dir = tmp_path / "accounts"
    account_dir.mkdir()
    account_file = account_dir / "paper-directional.env"
    account_file.write_text("ALPACA_API_KEY=key\n", encoding="utf-8")

    assert alpaca_env_file_for_profile("paper-directional", tmp_path) == account_file


def test_alpaca_env_file_for_profile_falls_back_to_main_env(tmp_path) -> None:
    assert alpaca_env_file_for_profile("paper-main", tmp_path) == tmp_path / "options-engine.env"


def test_load_alpaca_env_file_does_not_override_existing_by_default(tmp_path, monkeypatch) -> None:
    env_file = tmp_path / "profile.env"
    env_file.write_text(
        """
        # comment
        export ALPACA_API_KEY=file-key
        ALPACA_SECRET_KEY="file-secret"
        ALPACA_TRADING_BASE_URL=https://paper-api.alpaca.markets
        """,
        encoding="utf-8",
    )
    monkeypatch.setenv("ALPACA_API_KEY", "existing-key")

    loaded = load_alpaca_env_file(env_file)

    assert loaded["ALPACA_API_KEY"] == "file-key"
    assert loaded["ALPACA_SECRET_KEY"] == "file-secret"
    assert loaded["ALPACA_TRADING_BASE_URL"] == "https://paper-api.alpaca.markets"
    assert loaded_env("ALPACA_API_KEY") == "existing-key"


def test_load_alpaca_profile_from_args_sets_account_name(tmp_path, monkeypatch) -> None:
    monkeypatch.delenv("NAUTILUS_ALPACA_ACCOUNT", raising=False)
    account_dir = tmp_path / "accounts"
    account_dir.mkdir()
    env_file = account_dir / "paper-directional.env"
    env_file.write_text("ALPACA_API_KEY=file-key\n", encoding="utf-8")
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path.parent))

    config_home = tmp_path.parent / "nautilus-trader" / "alpaca"
    config_home.mkdir(parents=True)
    account_dir.rename(config_home / "accounts")

    args = argparse.Namespace(alpaca_profile="paper-directional", alpaca_env_file=None)

    path = load_alpaca_profile_from_args(args)

    assert path == config_home / "accounts" / "paper-directional.env"
    assert loaded_env("NAUTILUS_ALPACA_ACCOUNT") == "paper-directional"


def loaded_env(name: str) -> str:
    import os

    return os.environ[name]
