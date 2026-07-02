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

//! Syncs earnings-calendar observations into the scheduled-event catalog.

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Utc};
use nautilus_alpaca::earnings::{
    EarningsEntryPolicy, EarningsEvent, EarningsObservationProvenance, EarningsTiming,
    earnings_events_to_observations, format_earnings_events_csv,
    parse_alpha_vantage_earnings_calendar_csv, parse_earnings_events_csv,
};
use nautilus_persistence::backend::catalog::ParquetDataCatalog;
use nautilus_trading::scheduled_events::{
    ScheduledEventObservation, ensure_scheduled_event_custom_data_registered,
    scheduled_event_catalog_path, scheduled_event_raw_path,
};
use sha2::{Digest, Sha256};
use url::Url;

const DEFAULT_CACHE_SECS: u64 = 23 * 60 * 60;
const DEFAULT_HORIZON: &str = "3month";
const SOURCE_ALPHA_VANTAGE: &str = "alpha_vantage";
const SOURCE_MANUAL_OVERRIDE: &str = "manual_override";
const EVENT_TYPE_EARNINGS_REPORT: &str = "earnings_report";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let sync_time_utc = Utc::now();
    let args = Args::from_env(sync_time_utc)?;
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
            if args.raw_evidence_path != args.raw_path {
                write_atomic(&args.raw_evidence_path, &body)?;
            }
            body
        }
    };
    let alpha_vantage_raw_evidence_path = match raw_source {
        RawSource::Cache => args.raw_path.clone(),
        RawSource::Download => args.raw_evidence_path.clone(),
    };
    let alpha_vantage_fetched_at_utc = match raw_source {
        RawSource::Cache => file_modified_utc(&args.raw_path).unwrap_or(sync_time_utc),
        RawSource::Download => sync_time_utc,
    };
    let raw_sha256 = sha256_hex(raw.as_bytes());

    let events = parse_alpha_vantage_earnings_calendar_csv(&raw)?;
    let alpha_vantage_observations = earnings_events_to_observations(
        &events,
        &EarningsObservationProvenance {
            source: SOURCE_ALPHA_VANTAGE.to_string(),
            raw_uri: file_uri(&alpha_vantage_raw_evidence_path),
            raw_sha256: raw_sha256.clone(),
            source_fetched_at_utc: alpha_vantage_fetched_at_utc,
            source_published_at_utc: None,
        },
    );

    let manual_observations =
        load_manual_override_observations(args.manual_overrides_path.as_deref(), sync_time_utc)?;
    let catalog_path = args.catalog_path.clone();
    let observations = alpha_vantage_observations
        .into_iter()
        .chain(manual_observations.iter().cloned())
        .collect();
    let observation_count = tokio::task::spawn_blocking(move || {
        write_observations_to_catalog(&catalog_path, observations)
    })
    .await??;

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

    let manual_observation_count = manual_observations.len();
    let raw_counts = quality_counts(&events);
    let approved_counts = quality_counts(&approved_events);
    println!(
        "earnings_sync: source={} events={} before_open={} after_close={} unknown={} approved_events={} approved_before_open={} approved_after_close={} observations={} manual_observations={} catalog_path={} raw_evidence_path={} raw_sha256={} csv_bridge_output_path={} csv_bridge_approved_path={}",
        raw_source.as_str(),
        events.len(),
        raw_counts.before_open,
        raw_counts.after_close,
        raw_counts.unknown,
        approved_events.len(),
        approved_counts.before_open,
        approved_counts.after_close,
        observation_count,
        manual_observation_count,
        args.catalog_path.display(),
        alpha_vantage_raw_evidence_path.display(),
        raw_sha256,
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
    raw_evidence_path: PathBuf,
    catalog_path: PathBuf,
    manual_overrides_path: Option<PathBuf>,
    cache_secs: u64,
    force: bool,
    trade_date: chrono::NaiveDate,
}

impl Args {
    fn from_env(sync_time_utc: DateTime<Utc>) -> anyhow::Result<Self> {
        let mut output_path = env::var("ALPACA_EARNINGS_EVENTS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_cache_dir().join("earnings_events.csv"));
        let mut approved_path = env::var("ALPACA_EARNINGS_APPROVED_EVENTS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_cache_dir().join("earnings_events_approved.csv"));
        let mut raw_path = env::var("ALPHA_VANTAGE_EARNINGS_RAW_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_alpha_vantage_raw_cache_path());
        let mut raw_evidence_path = env::var("ALPHA_VANTAGE_EARNINGS_RAW_EVIDENCE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_alpha_vantage_raw_evidence_path(sync_time_utc));
        let mut catalog_path = scheduled_event_catalog_path();
        let mut manual_overrides_path = env::var("ALPACA_EARNINGS_MANUAL_OVERRIDES_PATH")
            .ok()
            .map(PathBuf::from);
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
                "--raw-evidence" => {
                    raw_evidence_path = PathBuf::from(
                        args.next()
                            .ok_or_else(|| anyhow::anyhow!("--raw-evidence requires a value"))?,
                    );
                }
                "--catalog-root" => {
                    catalog_path = PathBuf::from(
                        args.next()
                            .ok_or_else(|| anyhow::anyhow!("--catalog-root requires a value"))?,
                    );
                }
                "--manual-overrides" => {
                    manual_overrides_path =
                        Some(PathBuf::from(args.next().ok_or_else(|| {
                            anyhow::anyhow!("--manual-overrides requires a value")
                        })?));
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
            raw_evidence_path,
            catalog_path,
            manual_overrides_path,
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

fn load_manual_override_observations(
    path: Option<&Path>,
    sync_time_utc: DateTime<Utc>,
) -> anyhow::Result<Vec<ScheduledEventObservation>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };

    let raw = fs::read_to_string(path)?;
    let events = parse_earnings_events_csv(&raw)?;
    Ok(earnings_events_to_observations(
        &events,
        &EarningsObservationProvenance {
            source: SOURCE_MANUAL_OVERRIDE.to_string(),
            raw_uri: file_uri(path),
            raw_sha256: sha256_hex(raw.as_bytes()),
            source_fetched_at_utc: sync_time_utc,
            source_published_at_utc: None,
        },
    ))
}

fn write_observations_to_catalog(
    catalog_path: &Path,
    observations: Vec<ScheduledEventObservation>,
) -> anyhow::Result<usize> {
    let count = observations.len();
    if observations.is_empty() {
        return Ok(0);
    }

    ensure_scheduled_event_custom_data_registered();
    fs::create_dir_all(catalog_path)?;
    let catalog = ParquetDataCatalog::new(catalog_path, None, None, None, None);
    let mut by_identifier = BTreeMap::new();

    for observation in observations {
        let identifier = ScheduledEventObservation::catalog_identifier(
            &observation.event_type,
            &observation.source,
        );
        by_identifier
            .entry(identifier.clone())
            .or_insert_with(Vec::new)
            .push(observation.into_custom_data(Some(identifier)));
    }

    for data in by_identifier.into_values() {
        catalog.write_custom_data_batch(data, None, None, Some(true))?;
    }

    Ok(count)
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

fn file_modified_utc(path: &Path) -> Option<DateTime<Utc>> {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(DateTime::<Utc>::from)
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

fn sha256_hex(contents: &[u8]) -> String {
    let digest = Sha256::digest(contents);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn file_uri(path: &Path) -> String {
    path.canonicalize()
        .map(|path| format!("file://{}", path.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn default_alpha_vantage_raw_cache_path() -> PathBuf {
    scheduled_event_raw_path()
        .join(format!("source={SOURCE_ALPHA_VANTAGE}"))
        .join(format!("event_type={EVENT_TYPE_EARNINGS_REPORT}"))
        .join("cache.csv")
}

fn default_alpha_vantage_raw_evidence_path(sync_time_utc: DateTime<Utc>) -> PathBuf {
    scheduled_event_raw_path()
        .join(format!("source={SOURCE_ALPHA_VANTAGE}"))
        .join(format!("event_type={EVENT_TYPE_EARNINGS_REPORT}"))
        .join(format!("fetched_date={}", sync_time_utc.format("%Y-%m-%d")))
        .join(format!("{}.csv", sync_time_utc.format("%Y%m%dT%H%M%SZ")))
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
