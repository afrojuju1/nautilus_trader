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

use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use nautilus_common::actor::data_actor::{DataActorConfig, ImportableActorConfig};
use nautilus_core::python::to_pyruntime_err;
use nautilus_model::identifiers::{ActorId, StrategyId};
use nautilus_trading::ImportableStrategyConfig;
use pyo3::prelude::*;

use crate::{controller::Controller, trader::Trader};

/// Internal Python handle attached to user-authored controller instances.
#[pyclass(
    module = "nautilus_trader.core.nautilus_pyo3.trading",
    name = "_ControllerHandle",
    unsendable
)]
#[derive(Debug)]
pub struct PyControllerHandle {
    trader: Weak<RefCell<Trader>>,
    actor_id: ActorId,
}

impl PyControllerHandle {
    #[must_use]
    pub fn new(trader: &Rc<RefCell<Trader>>, actor_id: ActorId) -> Self {
        Self {
            trader: Rc::downgrade(trader),
            actor_id,
        }
    }

    fn controller(&self) -> PyResult<Controller> {
        let trader = self
            .trader
            .upgrade()
            .ok_or_else(|| to_pyruntime_err("Controller trader is no longer available"))?;

        Ok(Controller::new(
            trader,
            Some(DataActorConfig {
                actor_id: Some(self.actor_id),
                ..Default::default()
            }),
        ))
    }
}

#[pymethods]
impl PyControllerHandle {
    #[pyo3(name = "create_actor_from_config", signature = (actor_config, start=true))]
    #[expect(clippy::needless_pass_by_value)]
    fn py_create_actor_from_config(
        &self,
        actor_config: ImportableActorConfig,
        start: bool,
    ) -> PyResult<ActorId> {
        self.controller()?
            .create_actor_from_config(&actor_config, start)
            .map_err(to_pyruntime_err)
    }

    #[pyo3(name = "create_strategy_from_config", signature = (strategy_config, start=true))]
    #[expect(clippy::needless_pass_by_value)]
    fn py_create_strategy_from_config(
        &self,
        strategy_config: ImportableStrategyConfig,
        start: bool,
    ) -> PyResult<StrategyId> {
        self.controller()?
            .create_strategy_from_config(&strategy_config, start)
            .map_err(to_pyruntime_err)
    }

    #[pyo3(name = "start_actor")]
    fn py_start_actor(&self, actor_id: ActorId) -> PyResult<()> {
        self.controller()?
            .start_actor(&actor_id)
            .map_err(to_pyruntime_err)
    }

    #[pyo3(name = "stop_actor")]
    fn py_stop_actor(&self, actor_id: ActorId) -> PyResult<()> {
        self.controller()?
            .stop_actor(&actor_id)
            .map_err(to_pyruntime_err)
    }

    #[pyo3(name = "remove_actor")]
    fn py_remove_actor(&self, actor_id: ActorId) -> PyResult<()> {
        self.controller()?
            .remove_actor(&actor_id)
            .map_err(to_pyruntime_err)
    }

    #[pyo3(name = "start_strategy")]
    fn py_start_strategy(&self, strategy_id: StrategyId) -> PyResult<()> {
        self.controller()?
            .start_strategy(&strategy_id)
            .map_err(to_pyruntime_err)
    }

    #[pyo3(name = "stop_strategy")]
    fn py_stop_strategy(&self, strategy_id: StrategyId) -> PyResult<()> {
        self.controller()?
            .stop_strategy(&strategy_id)
            .map_err(to_pyruntime_err)
    }

    #[pyo3(name = "market_exit_strategy")]
    fn py_market_exit_strategy(&self, strategy_id: StrategyId) -> PyResult<()> {
        self.controller()?
            .exit_market(&strategy_id)
            .map_err(to_pyruntime_err)
    }

    #[pyo3(name = "remove_strategy")]
    fn py_remove_strategy(&self, strategy_id: StrategyId) -> PyResult<()> {
        self.controller()?
            .remove_strategy(&strategy_id)
            .map_err(to_pyruntime_err)
    }
}

pub(crate) fn attach_controller_handle(
    python_controller: &Py<PyAny>,
    trader: &Rc<RefCell<Trader>>,
    actor_id: ActorId,
) -> anyhow::Result<()> {
    Python::attach(|py| -> anyhow::Result<()> {
        let handle = Py::new(py, PyControllerHandle::new(trader, actor_id))?;
        let controller = python_controller.bind(py);

        if controller.hasattr("_set_controller_handle")? {
            controller.call_method1("_set_controller_handle", (handle,))?;
        } else {
            controller.setattr("_controller_handle", handle)?;
        }

        Ok(())
    })
}
