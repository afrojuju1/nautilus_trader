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
"""
Helpers for loading the shared Alpaca runtime env into Python examples.
"""

from __future__ import annotations

import argparse
import os
import re
from pathlib import Path


ENV_KEY_PATTERN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def alpaca_default_env_file(repo_root: Path | None = None) -> Path:
    """
    Return the shared repo-local Alpaca env file.
    """
    if explicit := os.getenv("NAUTILUS_ALPACA_ENV_FILE"):
        return Path(explicit).expanduser()

    if repo_root is not None:
        return repo_root.expanduser() / ".env"

    if repo := os.getenv("NAUTILUS_ALPACA_REPO"):
        return Path(repo).expanduser() / ".env"

    current = Path.cwd().resolve()
    for directory in (current, *current.parents):
        candidate = directory / ".env"
        if candidate.exists():
            return candidate
    return current / ".env"


def alpaca_env_file_for_profile(profile: str, repo_root: Path | None = None) -> Path:
    """
    Return the shared env file used by a Rust Alpaca runtime profile.

    Profiles select account identity only. They do not imply account-specific env files.
    """
    return alpaca_default_env_file(repo_root)


def load_alpaca_env_file(path: Path, *, override: bool = False) -> dict[str, str]:
    """
    Load simple KEY=VALUE lines from an Alpaca runtime env file into ``os.environ``.
    """
    env_path = path.expanduser()
    if not env_path.exists():
        raise FileNotFoundError(f"Alpaca env file does not exist: {env_path}")

    loaded = _parse_env_file(env_path)
    for key, value in loaded.items():
        if override or key not in os.environ:
            os.environ[key] = value
    return loaded


def add_alpaca_profile_args(parser: argparse.ArgumentParser) -> None:
    """
    Add shared account-profile flags to a Python Alpaca example parser.
    """
    parser.add_argument(
        "--alpaca-profile",
        help=(
            "Set NAUTILUS_ALPACA_ACCOUNT before building the node. Env values load from the "
            "repo-local .env unless --alpaca-env-file is passed."
        ),
    )
    parser.add_argument(
        "--alpaca-env-file",
        type=Path,
        help="Load a specific Alpaca runtime env file before building the node.",
    )


def load_alpaca_profile_from_args(
    args: argparse.Namespace,
    *,
    override: bool = False,
) -> Path | None:
    """
    Load the shared env file and apply any selected Alpaca account profile.
    """
    env_file = getattr(args, "alpaca_env_file", None)
    profile = getattr(args, "alpaca_profile", None)
    if env_file is None and profile is None:
        path = alpaca_default_env_file()
        if path.exists():
            load_alpaca_env_file(path, override=override)
            return path
        return None

    path = (
        Path(env_file).expanduser()
        if env_file is not None
        else alpaca_env_file_for_profile(profile)
    )
    load_alpaca_env_file(path, override=override)
    if profile is not None and (override or "NAUTILUS_ALPACA_ACCOUNT" not in os.environ):
        os.environ["NAUTILUS_ALPACA_ACCOUNT"] = profile
    return path


def _parse_env_file(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line_number, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line.removeprefix("export ").strip()
        if "=" not in line:
            continue

        key, value = line.split("=", maxsplit=1)
        key = key.strip()
        if not ENV_KEY_PATTERN.fullmatch(key):
            raise ValueError(f"Invalid env key {key!r} in {path}:{line_number}")

        values[key] = _strip_env_value(value.strip())
    return values


def _strip_env_value(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        return value[1:-1]
    return value
