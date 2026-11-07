//! Prometheus metrics (`GET /metrics`, text exposition format 0.0.4).
//!
//! Off unless `TEAMCRAFT_METRICS_TOKEN` is set; the scraper sends it as a
//! bearer token. Counts are kept in memory since the start of the process.
//! Labels are never taken from the request (no path or user), so the number
//! of series is fixed.

use std::{
    fmt::Write,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

/// Upper bounds (seconds) of the latency histogram.
pub const BUCKETS: [f64; 9] = [0.005, 0.025, 0.1, 0.25, 0.5, 1.0, 2.5, 10.0, 60.0];

const CLASSES: [&str; 5] = ["1xx", "2xx", "3xx", "4xx", "5xx"];

#[derive(Default)]
pub struct Metrics {
    by_class: [AtomicU64; 5],
    /// Cumulative: requests that took at most `BUCKETS[i]`.
    buckets: [AtomicU64; 9],
    count: AtomicU64,
    micros: AtomicU64,
}

impl Metrics {
    /// Records one answered request.
    pub fn record(&self, status: u16, took: Duration) {
        let class = usize::from(status / 100).clamp(1, 5) - 1;
        self.by_class[class].fetch_add(1, Ordering::Relaxed);
        let seconds = took.as_secs_f64();
        for (i, bound) in BUCKETS.iter().enumerate() {
            if seconds <= *bound {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        self.micros.fetch_add(
            u64::try_from(took.as_micros()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    /// The exposition text; `gauges` are `(name, help, value)` read by the
    /// caller (database pool, outbox backlogs).
    pub fn render(&self, version: &str, gauges: &[(&str, &str, f64)]) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# HELP teamcraft_build_info Version of the running server."
        );
        let _ = writeln!(out, "# TYPE teamcraft_build_info gauge");
        let _ = writeln!(out, "teamcraft_build_info{{version=\"{version}\"}} 1");
        let _ = writeln!(
            out,
            "# HELP teamcraft_http_requests_total Answered HTTP requests by status class."
        );
        let _ = writeln!(out, "# TYPE teamcraft_http_requests_total counter");
        for (i, class) in CLASSES.iter().enumerate() {
            let n = self.by_class[i].load(Ordering::Relaxed);
            let _ = writeln!(
                out,
                "teamcraft_http_requests_total{{class=\"{class}\"}} {n}"
            );
        }
        let _ = writeln!(
            out,
            "# HELP teamcraft_http_request_duration_seconds Time until the response headers were sent (a streamed body is not included)."
        );
        let _ = writeln!(
            out,
            "# TYPE teamcraft_http_request_duration_seconds histogram"
        );
        for (i, bound) in BUCKETS.iter().enumerate() {
            let n = self.buckets[i].load(Ordering::Relaxed);
            let _ = writeln!(
                out,
                "teamcraft_http_request_duration_seconds_bucket{{le=\"{bound}\"}} {n}"
            );
        }
        let count = self.count.load(Ordering::Relaxed);
        let _ = writeln!(
            out,
            "teamcraft_http_request_duration_seconds_bucket{{le=\"+Inf\"}} {count}"
        );
        let _ = writeln!(
            out,
            "teamcraft_http_request_duration_seconds_sum {}",
            self.micros.load(Ordering::Relaxed) as f64 / 1e6
        );
        let _ = writeln!(out, "teamcraft_http_request_duration_seconds_count {count}");
        for (name, help, value) in gauges {
            let _ = writeln!(out, "# HELP {name} {help}");
            let _ = writeln!(out, "# TYPE {name} gauge");
            let _ = writeln!(out, "{name} {value}");
        }
        out
    }
}

/// Compares the presented bearer token with the configured one without
/// stopping at the first difference. The scheme name is case-insensitive
/// (RFC 7235), so `bearer …` works as well.
pub fn token_matches(header: Option<&str>, expected: &str) -> bool {
    let Some(given) = header.and_then(|h| {
        let (scheme, rest) = h.split_at_checked(7)?;
        scheme.eq_ignore_ascii_case("Bearer ").then_some(rest)
    }) else {
        return false;
    };
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_by_class_and_fills_cumulative_buckets() {
        let m = Metrics::default();
        m.record(200, Duration::from_millis(3));
        m.record(404, Duration::from_millis(200));
        m.record(503, Duration::from_secs(90));
        let text = m.render("0.2.0", &[("teamcraft_db_pool_size", "Connections.", 5.0)]);
        assert!(text.contains("teamcraft_build_info{version=\"0.2.0\"} 1"));
        assert!(text.contains("teamcraft_http_requests_total{class=\"2xx\"} 1"));
        assert!(text.contains("teamcraft_http_requests_total{class=\"4xx\"} 1"));
        assert!(text.contains("teamcraft_http_requests_total{class=\"5xx\"} 1"));
        assert!(text.contains("teamcraft_http_requests_total{class=\"3xx\"} 0"));
        assert!(text.contains("le=\"0.005\"} 1"));
        assert!(text.contains("le=\"0.25\"} 2"));
        assert!(text.contains("le=\"60\"} 2"));
        assert!(text.contains("le=\"+Inf\"} 3"));
        assert!(text.contains("teamcraft_http_request_duration_seconds_count 3"));
        assert!(text.contains("teamcraft_db_pool_size 5"));
        // every sample line is `name{labels} value`
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            assert_eq!(line.split(' ').count(), 2, "{line}");
        }
    }

    #[test]
    fn odd_status_codes_stay_in_range() {
        let m = Metrics::default();
        m.record(0, Duration::ZERO);
        m.record(999, Duration::ZERO);
        let text = m.render("x", &[]);
        assert!(text.contains("class=\"1xx\"} 1") && text.contains("class=\"5xx\"} 1"));
    }

    #[test]
    fn token_comparison() {
        assert!(token_matches(Some("Bearer s3cret"), "s3cret"));
        assert!(token_matches(Some("bearer s3cret"), "s3cret"));
        assert!(token_matches(Some("BEARER s3cret"), "s3cret"));
        assert!(!token_matches(Some("Basic  s3cret"), "s3cret"));
        assert!(!token_matches(Some("Bear"), "s3cret"));
        // a multi-byte character across the scheme boundary is no panic
        assert!(!token_matches(Some("Bearerä s3cret"), "s3cret"));
        assert!(!token_matches(Some("Bearer s3cret2"), "s3cret"));
        assert!(!token_matches(Some("Bearer s3cre"), "s3cret"));
        assert!(!token_matches(Some("s3cret"), "s3cret"));
        assert!(!token_matches(None, "s3cret"));
        assert!(!token_matches(Some("Bearer "), "s3cret"));
    }
}
