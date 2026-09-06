//! Latency arithmetic.
//!
//! Percentiles rather than a mean, because the mean of a load test is the
//! one number that never describes anyone's experience: a hub that serves
//! 99% of reads in 3ms and the rest in 4 seconds has a fine mean and is
//! broken. Percentiles are computed by sorting the samples outright rather
//! than through a histogram -- a run holds tens of thousands of samples,
//! not billions, and an exact number that needs no error bar is worth more
//! here than the memory a sketch would save.

use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Duration;

/// One request, as it turned out.
#[derive(Debug, Clone)]
pub struct Sample {
    /// What kind of request this was -- `"GET /tasks"`, `"claim"`. The
    /// unit of the report, so it must not carry an id: a label per task id
    /// produces a report with one row per request.
    pub label: &'static str,
    pub status: u16,
    pub latency: Duration,
}

impl Sample {
    pub fn new(label: &'static str, status: u16, latency: Duration) -> Self {
        Self {
            label,
            status,
            latency,
        }
    }
}

/// What a set of samples sharing one label came to.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub label: String,
    pub requests: usize,
    /// 2xx as a fraction of `requests`. Not "errors", because a 409 on a
    /// second faucet claim and a 429 from a rate limiter are both correct
    /// behaviour and both non-2xx; `by_status` is where the judgement
    /// happens.
    pub success_rate: f64,
    /// Every status code seen, and how often. Kept whole rather than
    /// bucketed into 4xx/5xx: the drills turn on the difference between
    /// one 4xx and another.
    pub by_status: BTreeMap<u16, usize>,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// Requests of this label per second over the wall-clock window the
    /// caller measured, or `None` when there was no window to divide by.
    pub per_second: Option<f64>,
}

fn percentile(sorted: &[Duration], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    // Nearest-rank: the smallest sample at or above the fraction. No
    // interpolation, so every reported number is one that actually
    // happened.
    let rank = (fraction * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1].as_secs_f64() * 1000.0
}

/// Groups `samples` by label and summarises each group. `window` is the
/// wall-clock span the samples were collected over, used for the rates.
pub fn summarize(samples: &[Sample], window: Option<Duration>) -> Vec<Summary> {
    let mut grouped: BTreeMap<&'static str, Vec<&Sample>> = BTreeMap::new();
    for sample in samples {
        grouped.entry(sample.label).or_default().push(sample);
    }

    grouped
        .into_iter()
        .map(|(label, group)| {
            let mut latencies: Vec<Duration> = group.iter().map(|s| s.latency).collect();
            latencies.sort_unstable();

            let mut by_status: BTreeMap<u16, usize> = BTreeMap::new();
            for sample in &group {
                *by_status.entry(sample.status).or_default() += 1;
            }
            let successes = group
                .iter()
                .filter(|s| (200..300).contains(&s.status))
                .count();

            Summary {
                label: label.to_string(),
                requests: group.len(),
                success_rate: successes as f64 / group.len() as f64,
                by_status,
                p50_ms: percentile(&latencies, 0.50),
                p90_ms: percentile(&latencies, 0.90),
                p99_ms: percentile(&latencies, 0.99),
                max_ms: percentile(&latencies, 1.0),
                per_second: window.map(|w| group.len() as f64 / w.as_secs_f64()),
            }
        })
        .collect()
}

/// The summary table, as text. Kept next to the arithmetic so the text
/// report and the JSON one can never describe different numbers.
pub fn render_table(summaries: &[Summary]) -> String {
    let mut out = format!(
        "{:<28} {:>8} {:>7} {:>9} {:>9} {:>9} {:>9} {:>8}\n",
        "request", "count", "ok%", "p50 ms", "p90 ms", "p99 ms", "max ms", "req/s"
    );
    out.push_str(&"-".repeat(94));
    out.push('\n');
    for summary in summaries {
        out.push_str(&format!(
            "{:<28} {:>8} {:>6.1}% {:>9.1} {:>9.1} {:>9.1} {:>9.1} {:>8}\n",
            summary.label,
            summary.requests,
            summary.success_rate * 100.0,
            summary.p50_ms,
            summary.p90_ms,
            summary.p99_ms,
            summary.max_ms,
            summary
                .per_second
                .map(|r| format!("{r:.1}"))
                .unwrap_or_else(|| "-".to_string()),
        ));
        // Status codes go on their own line only when the request was not
        // uniformly successful -- otherwise every row carries a redundant
        // "200: 4013" and the table stops being readable at a glance.
        if summary.by_status.len() > 1 || !summary.by_status.contains_key(&200) {
            let codes: Vec<String> = summary
                .by_status
                .iter()
                .map(|(status, count)| format!("{status}×{count}"))
                .collect();
            out.push_str(&format!("{:<28} {}\n", "", codes.join("  ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(label: &'static str, status: u16, ms: u64) -> Sample {
        Sample::new(label, status, Duration::from_millis(ms))
    }

    #[test]
    fn percentiles_report_a_sample_that_actually_happened() {
        let latencies: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        // Nearest-rank on 1..=100: the 50th percentile is the 50th value.
        assert_eq!(percentile(&latencies, 0.50), 50.0);
        assert_eq!(percentile(&latencies, 0.99), 99.0);
        assert_eq!(percentile(&latencies, 1.0), 100.0);
    }

    #[test]
    fn percentiles_of_nothing_are_zero_rather_than_a_panic() {
        // A drill can legitimately record no samples for a label -- a
        // tier that was never exercised, a phase that ended early -- and
        // that must not take the report down with it.
        assert_eq!(percentile(&[], 0.99), 0.0);
    }

    #[test]
    fn summarize_keeps_every_status_code_apart() {
        let samples = vec![
            sample("claim", 200, 5),
            sample("claim", 429, 1),
            sample("claim", 409, 2),
        ];
        let summary = &summarize(&samples, None)[0];
        assert_eq!(summary.requests, 3);
        assert_eq!(summary.by_status[&429], 1);
        assert_eq!(summary.by_status[&409], 1);
        assert!((summary.success_rate - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn rates_are_absent_rather_than_infinite_without_a_window() {
        let summary = &summarize(&[sample("read", 200, 1)], None)[0];
        assert!(summary.per_second.is_none());
    }
}
