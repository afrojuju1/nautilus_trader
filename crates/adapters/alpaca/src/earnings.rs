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

//! Earnings-event input policy for Alpaca debit-spread strategy migration.
//!
//! This module intentionally consumes explicit earnings events supplied by an operator-approved
//! source. It does not infer earnings dates from option chains or broker snapshots.

use std::{fs, path::Path, str::FromStr};

use chrono::{Datelike, NaiveDate};
use thiserror::Error;

/// Earnings-calendar parsing and policy error.
#[derive(Debug, Error)]
pub enum EarningsCalendarError {
    /// The input did not include a header row.
    #[error("earnings calendar is missing a header row")]
    MissingHeader,
    /// A required column is missing from the header row.
    #[error("earnings calendar is missing required column `{0}`")]
    MissingColumn(&'static str),
    /// A data row did not have the same number of columns as the header.
    #[error("earnings calendar line {line} has {actual} columns, expected {expected}")]
    InvalidColumnCount {
        /// One-based input line number.
        line: usize,
        /// Expected column count.
        expected: usize,
        /// Actual column count.
        actual: usize,
    },
    /// A required field is empty.
    #[error("earnings calendar line {line} has empty `{field}`")]
    EmptyField {
        /// One-based input line number.
        line: usize,
        /// Field name.
        field: &'static str,
    },
    /// A report date could not be parsed.
    #[error("earnings calendar line {line} has invalid report_date `{value}`")]
    InvalidDate {
        /// One-based input line number.
        line: usize,
        /// Invalid date value.
        value: String,
    },
    /// A report timing value could not be parsed.
    #[error("earnings calendar line {line} has invalid timing `{value}`")]
    InvalidTiming {
        /// One-based input line number.
        line: usize,
        /// Invalid timing value.
        value: String,
    },
    /// An upstream row is missing a required source column.
    #[error("earnings calendar source row line {line} is missing `{field}`")]
    MissingSourceField {
        /// One-based input line number.
        line: usize,
        /// Field name.
        field: &'static str,
    },
    /// File I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Earnings report timing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EarningsTiming {
    /// Report is expected before the regular session opens.
    BeforeOpen,
    /// Report is expected after the regular session closes.
    AfterClose,
    /// Report timing is unknown.
    Unknown,
}

impl EarningsTiming {
    /// Returns the canonical lowercase label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeOpen => "before_open",
            Self::AfterClose => "after_close",
            Self::Unknown => "unknown",
        }
    }
}

impl FromStr for EarningsTiming {
    type Err = EarningsCalendarError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_timing(value).ok_or_else(|| EarningsCalendarError::InvalidTiming {
            line: 0,
            value: value.to_string(),
        })
    }
}

/// One approved earnings event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EarningsEvent {
    /// Uppercase underlying symbol.
    pub underlying: String,
    /// Report date in the exchange-local calendar.
    pub report_date: NaiveDate,
    /// Expected report timing.
    pub timing: EarningsTiming,
    /// Operator-approved event source label.
    pub source: String,
}

/// Entry policy for earnings debit-spread candidates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EarningsEntryPolicy {
    /// Earliest allowed entry, expressed as days before report date.
    pub max_days_before_report: i64,
    /// Latest allowed entry, expressed as days before report date.
    pub min_days_before_report: i64,
    /// Whether unknown report timing can be traded.
    pub allow_unknown_timing: bool,
    /// Whether weekend report dates can be traded.
    pub allow_weekend_reports: bool,
    /// Whether symbols outside the common listed-equity shape can be traded.
    pub allow_non_common_symbols: bool,
}

impl Default for EarningsEntryPolicy {
    fn default() -> Self {
        Self {
            max_days_before_report: 7,
            min_days_before_report: 0,
            allow_unknown_timing: false,
            allow_weekend_reports: false,
            allow_non_common_symbols: false,
        }
    }
}

impl EarningsEntryPolicy {
    /// Returns approved events eligible for a strategy entry on `trade_date`.
    #[must_use]
    pub fn eligible_events<'a>(
        &self,
        events: &'a [EarningsEvent],
        trade_date: NaiveDate,
    ) -> Vec<&'a EarningsEvent> {
        events
            .iter()
            .filter(|event| self.allow_unknown_timing || event.timing != EarningsTiming::Unknown)
            .filter(|event| self.allow_weekend_reports || is_weekday(event.report_date))
            .filter(|event| {
                self.allow_non_common_symbols || is_common_listed_equity_symbol(&event.underlying)
            })
            .filter(|event| {
                let days_before = days_to_report(event, trade_date);
                self.min_days_before_report <= days_before
                    && days_before <= self.max_days_before_report
            })
            .collect()
    }
}

/// Returns only events allowed by the default production input-quality policy.
#[must_use]
pub fn production_quality_events(
    events: &[EarningsEvent],
    trade_date: NaiveDate,
) -> Vec<&EarningsEvent> {
    EarningsEntryPolicy::default().eligible_events(events, trade_date)
}

/// Returns the calendar-day distance from `trade_date` to an earnings report.
#[must_use]
pub fn days_to_report(event: &EarningsEvent, trade_date: NaiveDate) -> i64 {
    event
        .report_date
        .signed_duration_since(trade_date)
        .num_days()
}

/// Returns whether an earnings report falls inside the configured event-shock window.
#[must_use]
pub const fn is_inside_event_shock_window(
    days_to_report: i64,
    block_days_before_earnings: i64,
    block_days_after_earnings: i64,
) -> bool {
    -block_days_after_earnings <= days_to_report && days_to_report <= block_days_before_earnings
}

/// Loads earnings events from a simple CSV file.
///
/// The CSV must have these columns: `underlying,report_date,timing,source`. Quoted fields and
/// embedded commas are intentionally unsupported; use stable source labels without commas.
///
/// # Errors
///
/// Returns an error when the file cannot be read or the CSV content is invalid.
pub fn load_earnings_events_csv(path: &Path) -> Result<Vec<EarningsEvent>, EarningsCalendarError> {
    parse_earnings_events_csv(&fs::read_to_string(path)?)
}

/// Parses earnings events from simple CSV text.
///
/// The CSV must have these columns: `underlying,report_date,timing,source`. Extra columns are
/// allowed and ignored.
///
/// # Errors
///
/// Returns an error when required columns are missing or any data row is invalid.
pub fn parse_earnings_events_csv(input: &str) -> Result<Vec<EarningsEvent>, EarningsCalendarError> {
    let mut rows = input
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'));

    let Some((_, header)) = rows.next() else {
        return Err(EarningsCalendarError::MissingHeader);
    };
    let headers = split_csv_row(header);
    let underlying_index = required_column(&headers, "underlying")?;
    let report_date_index = required_column(&headers, "report_date")?;
    let timing_index = required_column(&headers, "timing")?;
    let source_index = required_column(&headers, "source")?;

    let mut events = Vec::new();
    for (line, row) in rows {
        let columns = split_csv_row(row);
        if columns.len() != headers.len() {
            return Err(EarningsCalendarError::InvalidColumnCount {
                line,
                expected: headers.len(),
                actual: columns.len(),
            });
        }

        let underlying =
            required_field(&columns, underlying_index, line, "underlying")?.to_ascii_uppercase();
        let report_date = NaiveDate::parse_from_str(
            required_field(&columns, report_date_index, line, "report_date")?,
            "%Y-%m-%d",
        )
        .map_err(|_| EarningsCalendarError::InvalidDate {
            line,
            value: columns[report_date_index].to_string(),
        })?;
        let timing_value = required_field(&columns, timing_index, line, "timing")?;
        let timing =
            parse_timing(timing_value).ok_or_else(|| EarningsCalendarError::InvalidTiming {
                line,
                value: timing_value.to_string(),
            })?;
        let source = required_field(&columns, source_index, line, "source")?.to_string();

        events.push(EarningsEvent {
            underlying,
            report_date,
            timing,
            source,
        });
    }

    Ok(events)
}

/// Parses Alpha Vantage `EARNINGS_CALENDAR` CSV into normalized earnings events.
///
/// Alpha Vantage returns `symbol,name,reportDate,fiscalDateEnding,estimate,currency,timeOfTheDay`.
/// This adapter keeps only the event fields needed by the strategy input policy.
///
/// # Errors
///
/// Returns an error when required Alpha Vantage columns are missing or a data row is invalid.
pub fn parse_alpha_vantage_earnings_calendar_csv(
    input: &str,
) -> Result<Vec<EarningsEvent>, EarningsCalendarError> {
    let mut rows = input
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty());

    let Some((_, header)) = rows.next() else {
        return Err(EarningsCalendarError::MissingHeader);
    };
    let headers = split_csv_row(header);
    let symbol_index = required_column(&headers, "symbol")?;
    let report_date_index = required_column(&headers, "reportDate")?;
    let timing_index = required_column(&headers, "timeOfTheDay")?;

    let mut events = Vec::new();
    for (line, row) in rows {
        let columns = split_csv_row(row);
        let symbol = source_field(&columns, symbol_index, line, "symbol")?.to_ascii_uppercase();
        let report_date_value = source_field(&columns, report_date_index, line, "reportDate")?;
        let report_date =
            NaiveDate::parse_from_str(report_date_value, "%Y-%m-%d").map_err(|_| {
                EarningsCalendarError::InvalidDate {
                    line,
                    value: report_date_value.to_string(),
                }
            })?;
        let timing = alpha_vantage_timing(columns.get(timing_index).map(String::as_str))
            .ok_or_else(|| EarningsCalendarError::InvalidTiming {
                line,
                value: columns
                    .get(timing_index)
                    .cloned()
                    .unwrap_or_else(|| "<missing>".to_string()),
            })?;

        events.push(EarningsEvent {
            underlying: symbol,
            report_date,
            timing,
            source: "alpha_vantage".to_string(),
        });
    }

    Ok(events)
}

/// Formats earnings events as normalized CSV.
#[must_use]
pub fn format_earnings_events_csv(events: &[EarningsEvent]) -> String {
    let mut output = String::from("underlying,report_date,timing,source\n");
    for event in events {
        output.push_str(&format!(
            "{},{},{},{}\n",
            event.underlying,
            event.report_date,
            event.timing.as_str(),
            event.source
        ));
    }
    output
}

fn required_column(headers: &[String], name: &'static str) -> Result<usize, EarningsCalendarError> {
    headers
        .iter()
        .position(|header| header == name)
        .ok_or(EarningsCalendarError::MissingColumn(name))
}

fn required_field<'a>(
    columns: &'a [String],
    index: usize,
    line: usize,
    field: &'static str,
) -> Result<&'a str, EarningsCalendarError> {
    let value = columns[index].trim();
    if value.is_empty() {
        return Err(EarningsCalendarError::EmptyField { line, field });
    }
    Ok(value)
}

fn source_field<'a>(
    columns: &'a [String],
    index: usize,
    line: usize,
    field: &'static str,
) -> Result<&'a str, EarningsCalendarError> {
    columns
        .get(index)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or(EarningsCalendarError::MissingSourceField { line, field })
}

fn split_csv_row(row: &str) -> Vec<String> {
    let mut columns = Vec::new();
    let mut current = String::new();
    let mut chars = row.chars().peekable();
    let mut quoted = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
            }
            '"' => {
                quoted = !quoted;
            }
            ',' if !quoted => {
                columns.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    columns.push(current.trim().to_string());
    columns
}

fn parse_timing(value: &str) -> Option<EarningsTiming> {
    match value.trim().to_ascii_lowercase().as_str() {
        "before_open" | "before" | "bmo" => Some(EarningsTiming::BeforeOpen),
        "after_close" | "after" | "amc" => Some(EarningsTiming::AfterClose),
        "unknown" | "unk" => Some(EarningsTiming::Unknown),
        _ => None,
    }
}

fn alpha_vantage_timing(value: Option<&str>) -> Option<EarningsTiming> {
    let value = value.unwrap_or_default().trim();
    if value.is_empty() {
        return Some(EarningsTiming::Unknown);
    }
    match value.to_ascii_lowercase().as_str() {
        "pre-market" => Some(EarningsTiming::BeforeOpen),
        "post-market" => Some(EarningsTiming::AfterClose),
        _ => parse_timing(value),
    }
}

fn is_weekday(date: NaiveDate) -> bool {
    date.weekday().number_from_monday() <= 5
}

fn is_common_listed_equity_symbol(symbol: &str) -> bool {
    let len = symbol.len();
    (1..=4).contains(&len) && symbol.chars().all(|ch| ch.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_earnings_events_csv_accepts_approved_events() {
        let events = parse_earnings_events_csv(
            "underlying,report_date,timing,source\nSPY,2026-05-05,after_close,manual\nqqq,2026-05-06,bmo,manual\n",
        )
        .unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].underlying, "SPY");
        assert_eq!(events[0].timing, EarningsTiming::AfterClose);
        assert_eq!(events[1].underlying, "QQQ");
        assert_eq!(events[1].timing, EarningsTiming::BeforeOpen);
    }

    #[test]
    fn parse_earnings_events_csv_rejects_invalid_timing() {
        let error = parse_earnings_events_csv(
            "underlying,report_date,timing,source\nSPY,2026-05-05,during_session,manual\n",
        )
        .unwrap_err();

        assert!(matches!(
            error,
            EarningsCalendarError::InvalidTiming { line: 2, .. }
        ));
    }

    #[test]
    fn earnings_policy_filters_window_and_unknown_timing() {
        let events = parse_earnings_events_csv(
            "underlying,report_date,timing,source\nSPY,2026-05-05,after_close,manual\nQQQ,2026-05-12,after_close,manual\nIWM,2026-05-05,unknown,manual\n",
        )
        .unwrap();
        let policy = EarningsEntryPolicy::default();
        let trade_date = NaiveDate::from_ymd_opt(2026, 5, 4).unwrap();

        let eligible = policy.eligible_events(&events, trade_date);

        assert_eq!(
            eligible
                .iter()
                .map(|event| event.underlying.as_str())
                .collect::<Vec<_>>(),
            vec!["SPY"]
        );
    }

    #[test]
    fn earnings_policy_filters_weekends_and_non_common_symbols() {
        let events = parse_earnings_events_csv(
            "underlying,report_date,timing,source\nSPY,2026-05-05,after_close,manual\nBRK.B,2026-05-05,after_close,manual\nNABZY,2026-05-05,after_close,manual\nQQQ,2026-05-09,after_close,manual\n",
        )
        .unwrap();
        let policy = EarningsEntryPolicy::default();
        let trade_date = NaiveDate::from_ymd_opt(2026, 5, 4).unwrap();

        let eligible = policy.eligible_events(&events, trade_date);

        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].underlying, "SPY");
    }

    #[test]
    fn earnings_policy_can_allow_unknown_timing() {
        let events = parse_earnings_events_csv(
            "underlying,report_date,timing,source\nIWM,2026-05-05,unknown,manual\n",
        )
        .unwrap();
        let policy = EarningsEntryPolicy {
            allow_unknown_timing: true,
            ..EarningsEntryPolicy::default()
        };
        let trade_date = NaiveDate::from_ymd_opt(2026, 5, 4).unwrap();

        assert_eq!(policy.eligible_events(&events, trade_date).len(), 1);
    }

    #[test]
    fn parse_alpha_vantage_calendar_handles_quoted_company_names() {
        let events = parse_alpha_vantage_earnings_calendar_csv(
            "symbol,name,reportDate,fiscalDateEnding,estimate,currency,timeOfTheDay\nBCC,\"BOISE CASCADE, L.L.C.\",2026-05-04,2026-03-31,0.43,USD,post-market\nADCT,ADC THERAPEUTICS SA,2026-05-04,2026-03-31,-0.19,USD,pre-market\nNABZY,NABZY,2026-05-03,2026-03-31,,USD,\n",
        )
        .unwrap();

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].underlying, "BCC");
        assert_eq!(
            events[0].report_date,
            NaiveDate::from_ymd_opt(2026, 5, 4).unwrap()
        );
        assert_eq!(events[0].timing, EarningsTiming::AfterClose);
        assert_eq!(events[1].timing, EarningsTiming::BeforeOpen);
        assert_eq!(events[2].timing, EarningsTiming::Unknown);
    }

    #[test]
    fn format_earnings_events_csv_round_trips_normalized_events() {
        let events = parse_alpha_vantage_earnings_calendar_csv(
            "symbol,name,reportDate,fiscalDateEnding,estimate,currency,timeOfTheDay\nADCT,ADC THERAPEUTICS SA,2026-05-04,2026-03-31,-0.19,USD,pre-market\n",
        )
        .unwrap();
        let normalized = format_earnings_events_csv(&events);

        assert_eq!(parse_earnings_events_csv(&normalized).unwrap(), events);
    }
}
