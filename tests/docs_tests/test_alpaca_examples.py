from __future__ import annotations

import re
import tomllib
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
ALPACA_CARGO_TOML = REPO_ROOT / "crates/adapters/alpaca/Cargo.toml"
ALPACA_EXAMPLE_README = REPO_ROOT / "examples/live/alpaca/README.md"
ALPACA_INTEGRATION_DOC = REPO_ROOT / "docs/integrations/alpaca.md"

BIN_PATTERN = re.compile(r"--bin\s+([a-z0-9-]+)")


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def _alpaca_bins() -> set[str]:
    with ALPACA_CARGO_TOML.open("rb") as cargo_toml:
        metadata = tomllib.load(cargo_toml)

    return {binary["name"] for binary in metadata.get("bin", [])}


def _referenced_bins(*paths: Path) -> set[str]:
    referenced: set[str] = set()
    for path in paths:
        referenced.update(BIN_PATTERN.findall(_read(path)))

    return referenced


def test_alpaca_docs_reference_existing_runtime_bins() -> None:
    referenced = _referenced_bins(ALPACA_EXAMPLE_README, ALPACA_INTEGRATION_DOC)

    assert {
        "alpaca-check-account-orders",
        "alpaca-load-option-contracts",
        "alpaca-load-option-snapshots",
        "alpaca-compare-option-chain-scan",
        "alpaca-validate-mleg-order",
        "alpaca-paper-execution-harness",
        "alpaca-options-engine",
    } <= referenced
    assert referenced <= _alpaca_bins()


def test_alpaca_paper_smoke_examples_keep_submit_cancel_policy() -> None:
    examples = _read(ALPACA_EXAMPLE_README)
    integration_doc = _read(ALPACA_INTEGRATION_DOC)
    smoke_section = examples.split("## Rust operator submit/cancel diagnostic", maxsplit=1)[1]
    smoke_section = smoke_section.split("\n## ", maxsplit=1)[0]

    assert "alpaca-paper-execution-harness" in smoke_section
    assert "alpaca-check-account-orders" in smoke_section
    assert smoke_section.index("alpaca-paper-execution-harness") < smoke_section.index(
        "alpaca-check-account-orders",
    )
    assert "Do not leave smoke orders working" in smoke_section
    assert "alpaca-submit-mleg-order" not in examples
    assert "alpaca-submit-mleg-order" not in integration_doc


def test_alpaca_docs_keep_public_examples_non_submitting_by_default() -> None:
    examples = _read(ALPACA_EXAMPLE_README)
    integration_doc = _read(ALPACA_INTEGRATION_DOC)

    assert "ALPACA_SUBMIT=false" in examples
    assert "ALPACA_MANAGE=false" in examples
    assert "ALPACA_CLOSE=false" in examples
    assert "--check-config" in examples
    assert "--check-config" in integration_doc


def test_alpaca_docs_use_current_sample_option_contracts() -> None:
    examples = _read(ALPACA_EXAMPLE_README)
    integration_doc = _read(ALPACA_INTEGRATION_DOC)
    combined_docs = f"{examples}\n{integration_doc}"

    assert "SPY260619P00450000" in combined_docs
    assert "SPY260619P00445000" in combined_docs
    assert "2026-06-19" in combined_docs
    for stale_sample in ("2026-01", "260116", "260123"):
        assert stale_sample not in combined_docs
