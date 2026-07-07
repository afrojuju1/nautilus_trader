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

from __future__ import annotations

from typing import Any

from nautilus_trader._libnautilus.trading import ImportableStrategyConfig
from nautilus_trader.common import DataActor
from nautilus_trader.common import ImportableActorConfig
from nautilus_trader.model import ActorId
from nautilus_trader.model import StrategyId


class Controller(DataActor):
    def __init__(self, config: Any | None = None) -> None:
        super().__init__(config)
        self._controller_handle: Any | None = None

    def _set_controller_handle(self, handle: Any) -> None:
        self._controller_handle = handle

    def _handle(self) -> Any:
        if self._controller_handle is None:
            raise RuntimeError("Controller is not registered with a trader")
        return self._controller_handle

    def create_actor_from_config(
        self,
        actor_config: ImportableActorConfig,
        start: bool = True,
    ) -> ActorId:
        return self._handle().create_actor_from_config(actor_config, start)

    def create_strategy_from_config(
        self,
        strategy_config: ImportableStrategyConfig,
        start: bool = True,
    ) -> StrategyId:
        return self._handle().create_strategy_from_config(strategy_config, start)

    def start_actor(self, actor_id: ActorId) -> None:
        self._handle().start_actor(actor_id)

    def start_actor_from_id(self, actor_id: ActorId) -> None:
        self.start_actor(actor_id)

    def stop_actor(self, actor_id: ActorId) -> None:
        self._handle().stop_actor(actor_id)

    def stop_actor_from_id(self, actor_id: ActorId) -> None:
        self.stop_actor(actor_id)

    def remove_actor(self, actor_id: ActorId) -> None:
        self._handle().remove_actor(actor_id)

    def remove_actor_from_id(self, actor_id: ActorId) -> None:
        self.remove_actor(actor_id)

    def start_strategy(self, strategy_id: StrategyId) -> None:
        self._handle().start_strategy(strategy_id)

    def start_strategy_from_id(self, strategy_id: StrategyId) -> None:
        self.start_strategy(strategy_id)

    def stop_strategy(self, strategy_id: StrategyId) -> None:
        self._handle().stop_strategy(strategy_id)

    def stop_strategy_from_id(self, strategy_id: StrategyId) -> None:
        self.stop_strategy(strategy_id)

    def market_exit_strategy(self, strategy_id: StrategyId) -> None:
        self._handle().market_exit_strategy(strategy_id)

    def market_exit_strategy_from_id(self, strategy_id: StrategyId) -> None:
        self.market_exit_strategy(strategy_id)

    def remove_strategy(self, strategy_id: StrategyId) -> None:
        self._handle().remove_strategy(strategy_id)

    def remove_strategy_from_id(self, strategy_id: StrategyId) -> None:
        self.remove_strategy(strategy_id)
