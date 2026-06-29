# Alpaca

This page documents the Python-facing Alpaca modules currently present in
`nautilus_trader.adapters.alpaca`.

:::warning
The current Alpaca integration is still an experimental adapter/runtime slice, not full Alpaca API
parity. The Python `TradingNode` factories are implemented for the narrow documented surface:
static US equity instruments, exact OCC option instruments, stock bars, option snapshot
quotes/Greeks, simple equity/ETF `DAY` limit orders, and option multi-leg order lists.
:::

## Package

```{eval-rst}
.. automodule:: nautilus_trader.adapters.alpaca
   :no-index:
```

## Constants

```{eval-rst}
.. automodule:: nautilus_trader.adapters.alpaca.constants
   :show-inheritance:
   :members:
   :member-order: bysource
```

## Config

The config classes are available to Python users for the documented `TradingNode` surface and are
also shared with the Rust options runtime.

```{eval-rst}
.. automodule:: nautilus_trader.adapters.alpaca.config
   :show-inheritance:
   :members:
   :member-order: bysource
```

## Factories

The Python factory classes create the standard Alpaca Python data and execution clients:

```python
from nautilus_trader.adapters.alpaca.factories import AlpacaLiveDataClientFactory
from nautilus_trader.adapters.alpaca.factories import AlpacaLiveExecClientFactory
```

## Strategies

The exported Alpaca put-credit strategy module is a scanner scaffold, not the canonical execution
architecture. The standard option execution smoke path is the Python options multi-leg
`TradingNode` example, which submits normal Nautilus `SubmitOrderList` commands through the Alpaca
execution client.

```{eval-rst}
.. automodule:: nautilus_trader.adapters.alpaca.strategies
   :show-inheritance:
   :members:
   :no-index:
   :member-order: bysource
```

### Put credit

```{eval-rst}
.. automodule:: nautilus_trader.adapters.alpaca.strategies.put_credit
   :show-inheritance:
   :members:
   :member-order: bysource
```

## PyO3 scanner bindings

The current Rust-backed scanner bindings are exposed through:

```python
from nautilus_trader.core.nautilus_pyo3.alpaca import AlpacaPutCreditCandidate
from nautilus_trader.core.nautilus_pyo3.alpaca import AlpacaPutCreditScanResult
from nautilus_trader.core.nautilus_pyo3.alpaca import AlpacaPutCreditScannerConfig
from nautilus_trader.core.nautilus_pyo3.alpaca import scan_put_credit_once
```

These bindings run a synchronous put-credit scan over Alpaca option contracts and snapshots. They
are intentionally narrower than a full live data client.
