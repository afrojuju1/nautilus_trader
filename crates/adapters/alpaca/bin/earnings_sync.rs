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

//! Syncs Alpha Vantage earnings-calendar data into the local normalized earnings CSV cache.

use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use nautilus_alpaca::earnings::{
    EarningsEntryPolicy, EarningsEvent, EarningsTiming, format_earnings_events_csv,
    parse_alpha_vantage_earnings_calendar_csv,
};
use url::Url;

const DEFAULT_CACHE_SECS: u64 = 23 * 60 * 60;
const DEFAULT_HORIZON: &str = "3month";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::from_env()?;
    let raw_source = if !args.force && is_fresh(&args.raw_path, args.cache_secs) {
        RawSource::Cache
    } else {
        RawSource::Download
    };

    let raw = match raw_source {
        RawSource::Cache => fs::read_to_string(&args.raw_path)?,
        RawSource::Download => {
            let body = fetch_alpha_vantage_calendar(&args.api_key, &args.horizon).await?;
            write_atomic(&args.raw_path, &body)?;
            body
        }
    };

    let events = parse_alpha_vantage_earnings_calendar_csv(&raw)?;
    write_atomic(&args.output_path, &format_earnings_events_csv(&events))?;
    let approved_events = EarningsEntryPolicy::default()
        .eligible_events(&events, args.trade_date)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    write_atomic(
        &args.approved_path,
        &format_earnings_events_csv(&approved_events),
    )?;

    let raw_counts = quality_counts(&events);
    let approved_counts = quality_counts(&approved_events);
    println!(
        "earnings_sync: source={} events={} before_open={} after_close={} unknown={} approved_events={} approved_before_open={} approved_after_close={} raw_path={} output_path={} approved_path={}",
        raw_source.as_str(),
        events.len(),
        raw_counts.before_open,
        raw_counts.after_close,
        raw_counts.unknown,
        approved_events.len(),
        approved_counts.before_open,
        approved_counts.after_close,
        args.raw_path.display(),
        args.output_path.display(),
        args.approved_path.display(),
    );

    Ok(())
}

#[derive(Clone, Debug)]
struct Args {
    api_key: String,
    horizon: String,
    output_path: PathBuf,
    approved_path: PathBuf,
    raw_path: PathBuf,
    cache_secs: u64,
    force: bool,
    trade_date: chrono::NaiveDate,
}

impl Args {
    fn from_env() -> anyhow::Result<Self> {
        let mut output_path = env::var("ALPACA_EARNINGS_EVENTS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_cache_dir().join("earnings_events.csv"));
        let mut approved_path = env::var("ALPACA_EARNINGS_APPROVED_EVENTS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_cache_dir().join("earnings_events_approved.csv"));
        let mut raw_path = env::var("ALPHA_VANTAGE_EARNINGS_RAW_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                default_cache_dir().join("alpha_vantage_earnings_calendar_raw.csv")
            });
        let mut horizon = env::var("ALPHA_VANTAGE_EARNINGS_HORIZON")
            .unwrap_or_else(|_| DEFAULT_HORIZON.to_string());
        let cache_secs = env::var("ALPHA_VANTAGE_EARNINGS_CACHE_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_CACHE_SECS);
        let mut force = false;
        let mut trade_date = chrono::Utc::now().date_naive();

        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--force" => force = true,
                "--horizon" => {
                    horizon = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--horizon requires a value"))?;
                }
                "--output" => {
                    output_path = PathBuf::from(
                        args.next()
                            .ok_or_else(|| anyhow::anyhow!("--output requires a value"))?,
                    );
                }
                "--approved-output" => {
                    approved_path =
                        PathBuf::from(args.next().ok_or_else(|| {
                            anyhow::anyhow!("--approved-output requires a value")
                        })?);
                }
                "--raw" => {
                    raw_path = PathBuf::from(
                        args.next()
                            .ok_or_else(|| anyhow::anyhow!("--raw requires a value"))?,
                    );
                }
                "--trade-date" => {
                    trade_date = args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--trade-date requires a value"))?
                        .parse::<chrono::NaiveDate>()?;
                }
                _ => anyhow::bail!("unknown argument `{arg}`"),
            }
        }

        let api_key = env::var("ALPHA_VANTAGE_API_KEY")
            .map_err(|_| anyhow::anyhow!("ALPHA_VANTAGE_API_KEY is required"))?;
        Ok(Self {
            api_key,
            horizon,
            output_path,
            approved_path,
            raw_path,
            cache_secs,
            force,
            trade_date,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct QualityCounts {
    before_open: usize,
    after_close: usize,
    unknown: usize,
}

fn quality_counts(events: &[EarningsEvent]) -> QualityCounts {
    let mut counts = QualityCounts::default();
    for event in events {
        match event.timing {
            EarningsTiming::BeforeOpen => counts.before_open += 1,
            EarningsTiming::AfterClose => counts.after_close += 1,
            EarningsTiming::Unknown => counts.unknown += 1,
        }
    }
    counts
}

#[derive(Clone, Copy, Debug)]
enum RawSource {
    Cache,
    Download,
}

impl RawSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Cache => "cache",
            Self::Download => "download",
        }
    }
}

async fn fetch_alpha_vantage_calendar(api_key: &str, horizon: &str) -> anyhow::Result<String> {
    let url = Url::parse_with_params(
        "https://www.alphavantage.co/query",
        [
            ("function", "EARNINGS_CALENDAR"),
            ("horizon", horizon),
            ("apikey", api_key),
        ],
    )?;
    let response = reqwest::get(url).await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("Alpha Vantage request failed with HTTP {status}");
    }
    if !body.starts_with("symbol,") {
        anyhow::bail!("Alpha Vantage returned an unexpected payload; raw cache was not updated");
    }
    Ok(body)
}

fn is_fresh(path: &Path, cache_secs: u64) -> bool {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < Duration::from_secs(cache_secs))
}

fn write_atomic(path: &Path, contents: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, contents)?;
    fs::rename(tmp_path, path)?;
    Ok(())
}

fn default_cache_dir() -> PathBuf {
    if let Ok(value) = env::var("XDG_STATE_HOME") {
        return PathBuf::from(value)
            .join("nautilus_trader")
            .join("earnings");
    }
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".local")
        .join("state")
        .join("nautilus_trader")
        .join("earnings")
}
