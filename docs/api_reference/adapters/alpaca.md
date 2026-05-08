# Alpaca

This page documents the Python-facing Alpaca modules currently present in
`nautilus_trader.adapters.alpaca`.

:::warning
The current Alpaca integration is an experimental Rust options runtime. The Python
`AlpacaLiveDataClientFactory` and `AlpacaLiveExecClientFactory` are placeholders and should not be
registered with a live `TradingNode` yet. Use `docs/integrations/alpaca.md` for the implemented
runtime scope and safe read-only/dry-run examples.
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

The config classes are available to Python users today, but they are shared with the Rust options
runtime and future live clients. They should not be read as proof that the standard Python
`TradingNode` factories are ready.

```{eval-rst}
.. automodule:: nautilus_trader.adapters.alpaca.config
   :show-inheritance:
   :members:
   :member-order: bysource
```

## Factories

The Python factory classes are importable but deliberately raise `NotImplementedError` until the
standard Python live-client path is implemented:

```python
from nautilus_trader.adapters.alpaca.factories import AlpacaLiveDataClientFactory
from nautilus_trader.adapters.alpaca.factories import AlpacaLiveExecClientFactory
```

## Strategies

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
